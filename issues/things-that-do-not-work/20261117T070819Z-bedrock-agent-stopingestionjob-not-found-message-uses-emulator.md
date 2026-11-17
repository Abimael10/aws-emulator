Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-ingestion-job-lifecycle
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-ingestion-job-lifecycle
Issue-Shape-Id: com.amazonaws.bedrockagent#AmazonBedrockAgentBuildTimeLambda
Issue-Key: bedrock-agent|StopIngestionJob|4xx->4xx|$.message
Issue-Commit: 167cfdc2b02e0826c0e9b36858672171a14f9dee

# bedrock-agent StopIngestionJob not-found message uses emulator wording instead of the AWS IngestionJob template

> _Note: this issue was identified against the code base at commit `167cfdc2b02e0826c0e9b36858672171a14f9dee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopIngestionJob

## Description
The emulator reaches the right status and error type on StopIngestionJob's not-found path (404 ResourceNotFoundException) but serves the message member in its own hand-written lowercase voice instead of the sentence AWS words for this error class. The error message is part of the contract, so this is a wire divergence even though the status is right. 1 cause.

Cause 1 of 1: StopIngestionJob against an ingestionJobId that does not exist under an existing knowledge base / data source (fixture bedrock-agent-ingestion-job-lifecycle, step task#6).
AWS: Smithy: StopIngestionJob (POST /knowledgebases/{knowledgeBaseId}/datasources/{dataSourceId}/ingestionjobs/{ingestionJobId}/stop, success code 202) binds ResourceNotFoundException (errorKind client, httpError 404) with a message member; knowledgeBaseId/dataSourceId/ingestionJobId are all required @httpLabel members of pattern ^[0-9a-zA-Z]{10}$. The ingestion-job wording for this error class is "IngestionJob with id <ingestionJobId> is not found." — the fixed text is "IngestionJob with id " and " is not found."; the only variable part is the ingestionJobId taken from the request path. The enclosing knowledgeBaseId and dataSourceId are NOT part of this message.
Emulator: 404 with message 'could not find ingestion job with identifier <ingestionJobId> for data source <dataSourceId> in knowledge base <knowledgeBaseId>' — different fixed text, different capitalisation, and it appends the two enclosing resource ids that AWS omits.
Why: a client that matches on the message to distinguish a missing ingestion job from other 404s, or that surfaces the message to an operator or a runbook, sees text no AWS-targeting code recognises; the extra ids also make the string unparseable by anything keyed on the AWS template.
Fix: the StopIngestionJob handler's not-found error construction — i.e. the shared bedrock-agent not-found error helper that formats the ResourceNotFoundException message, which today formats it per-handler in the emulator's own voice.
Traces: bedrock-agent-ingestion-job-lifecycle#task#6.

Related: open issue 20261029T172025Z-bedrock-agent-resourcenotfoundexception-messages-use-emulator-wording-instead.md files the same root cause for GetFlow, GetIngestionJob, GetKnowledgeBase and GetDataSource. It does NOT carry StopIngestionJob's replay key, so this operation is net-new and needs its own pointer; fixing the shared helper will likely close both.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture bedrock-agent-ingestion-job-lifecycle, step task#6, operation StopIngestionJob. Status matched (expected 404, got 404); only the message diverged:

  $.message: value_mismatch (expected 'IngestionJob with id NOSUCHJOB1 is not found.', got 'could not find ingestion job with identifier NOSUCHJOB1 for data source F580PjlGVf in knowledge base 3lf8JGRyIY')

Required: HTTP 404, error code ResourceNotFoundException, message 'IngestionJob with id <ingestionJobId> is not found.' Fixed text: "IngestionJob with id " + " is not found.". Variable part: the ingestionJobId from the request path. The literal values in the recording (NOSUCHJOB1 as the requested job id, F580PjlGVf and 3lf8JGRyIY as this run's data source and knowledge base ids) are the probe's request/state values filling the template — they are not part of the requirement, and the latter two must not appear in the message at all.

Smithy (com.amazonaws.bedrockagent#AmazonBedrockAgentBuildTimeLambda): StopIngestionJob errors = AccessDeniedException(403), ConflictException(409), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400); ResourceNotFoundException { message } with errorKind client / httpError 404.

## Steps to Reproduce
1. Create a knowledge base and a CUSTOM data source in the emulator (as fixture bedrock-agent-ingestion-job-lifecycle does in setup).
2. Replay bedrock-agent-ingestion-job-lifecycle step task#6: POST /knowledgebases/{knowledgeBaseId}/datasources/{dataSourceId}/ingestionjobs/NOSUCHJOB1/stop with an ingestionJobId that does not exist.
3. Observe 404 ResourceNotFoundException and compare the body's message against the AWS template 'IngestionJob with id <ingestionJobId> is not found.' — the emulator returns 'could not find ingestion job with identifier <id> for data source <dsId> in knowledge base <kbId>'.
