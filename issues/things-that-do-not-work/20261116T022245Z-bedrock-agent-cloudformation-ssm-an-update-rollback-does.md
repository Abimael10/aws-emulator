Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent__cloudformation__ssm.kbcfn-embedding-model-replacement-rejected-while-kb-ids-imported
Issue-Key: bedrock-agent__cloudformation__ssm.kbcfn-embedding-model-replacement-rejected-while-kb-ids-imported|run#14|AssertionError|assert after == {f"{kb['stack']}-{k}": v for k, v in before.items()}, (
Issue-Commit: f6f16de4d463b0ffce56b8f6eb39771090126fd8

# bedrock-agent__cloudformation__ssm: an UPDATE_ROLLBACK does not restore the stack's Outputs/Exports — ListExports keeps the rolled-back replacement resources' values

> _Note: this issue was identified against the code base at commit `f6f16de4d463b0ffce56b8f6eb39771090126fd8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
ONE net-new root cause of service `bedrock-agent__cloudformation__ssm` (the heading the suite report lists the failing check under). It is grounded in a failing behavioral check the replay report left UNMARKED (neither ALREADY FILED nor OWNED).

Context on what is already filed: open issue 20261107T053030Z-bedrock-agent-cloudformation-ssm-2-net-new-causes.md carries this same check id as a replay key, but for TWO earlier assertions in the same check — that a value-preserving in-place update was wrongly cancelled (Cause 1 there) and that the 'Cannot update export … as it is in use by …' cancellation reason was recorded three times instead of once (Cause 2 there). Both of those assertions now PASS in this replay: the update is cancelled, the status is UPDATE_ROLLBACK_COMPLETE, and exactly one cancellation reason is recorded, naming the right export and the right importer. The check now fails several asserts LATER, on the post-rollback export VALUES, which neither that issue nor any other open issue records. That is why the report did not mark this failure OWNED, and why the cause below is new.

=== Cause 1 of 1: a cancelled, rolled-back UpdateStack leaves the stack's Outputs/Exports pointing at the replacement resources it created and then deleted, instead of restoring the pre-update values ===

AWS: recorded behavior only — the cloudformation Smithy model expresses nothing about rollback semantics; it supplies only the operations used to observe them (Smithy: UpdateStack(StackName required, TemplateBody, Parameters, Capabilities) -> StackId, modeled errors InsufficientCapabilitiesException / TokenAlreadyExistsException only; ListExports -> Exports (list of Export{Name, Value, ExportingStackId}), paginated on NextToken; DescribeStacks -> Stacks[].Outputs[].OutputKey/OutputValue). Recorded behavior: an UpdateStack that is cancelled and rolled back is ABSENT in its effect. When the update is cancelled because a replacing change would move an in-use export, CloudFormation reverts to the previous template and the previous physical resources, deletes the replacement resources it provisioned during the forward pass, and re-resolves the stack's Outputs from those previous resources — so after UPDATE_ROLLBACK_COMPLETE every export under that stack holds EXACTLY the value it held before the update was submitted, and ListExports reports those pre-update values under the same export names. The Export names themselves are `<stack-name>-<OutputKey>` here only because the template's Export.Name is Fn::Sub '${AWS::StackName}-<OutputKey>'; the requirement is value identity across the rollback, not any particular id string.

Emulator: the status (UPDATE_ROLLBACK_COMPLETE) and the single cancellation reason are correct, but the stack's exports are NOT reverted. After the rollback, ListExports reports, for both `<kb-stack>-MetadataKBKnowledgeBaseId` and `<kb-stack>-EscalationWriteupsKBKnowledgeBaseId`, the knowledgeBaseIds that were minted for the *replacement* (Generation=g2, cohere.embed-english-v3) knowledge bases created during the forward pass — not the ids the stack exported before the update. In the replay the two exports moved from the pre-update pair of ids to a different pair of ids. Hypothesis: the rollback path reverts resource state and emits the rollback events but never re-resolves and re-publishes the stack's Outputs/Exports from the restored (previous) resource set, so the export table keeps whatever the forward pass last wrote into it.

Why: a client relying on AWS's behavior treats a rolled-back update as a no-op and keeps consuming the exporting stack through Fn::ImportValue / ListExports. Against the emulator the export now names a resource that the rollback deleted (or that never became the stack's committed resource), so every consumer that resolves the export afterwards — a new importer stack, a re-deploy of the existing importer, or any tool reading ListExports — wires itself to a dangling knowledge-base id while the stack's real, surviving knowledge bases are unreachable through the export. This is exactly the corruption cross-stack export protection exists to prevent: the guard fires, the update is refused, and the export is still broken.

Fix: the UPDATE_ROLLBACK path of the cloudformation service's stack-update engine (services/service-com-amazonaws-cloudformation-cloudformation) — after reverting resources, re-resolve the previous template's Outputs against the restored physical resources and rewrite the stack's Outputs and the account-level export table from that resolution (and leave no export entry authored by the abandoned forward pass), so a cancelled update leaves export names and values byte-identical to their pre-update state.

Traces: bedrock-agent__cloudformation__ssm.kbcfn-embedding-model-replacement-rejected-while-kb-ids-imported (check line 62).

## Evidence
Cause 1 — bedrock-agent__cloudformation__ssm.kbcfn-embedding-model-replacement-rejected-while-kb-ids-imported [mismatch]:

Passing prefix (so these are NOT the defect): the UpdateStack that changes the embedding model of an imported KB stack ends UPDATE_ROLLBACK_COMPLETE, and exactly one stack-level ResourceStatusReason matches 'Cannot update export <export> as it is in use by <importer>.', naming one of the KB stack's exports and the agent stack as importer.

Failure, at check line 62:
  AssertionError: exports moved across a rolled-back update: {'EscalationWriteupsKBKnowledgeBaseId': 'fBNMCMnc9J', 'MetadataKBKnowledgeBaseId': 'zjrxjBv9Tr'} -> {'sim2real-kbcfn-0d981e2a1d3c-bedrock-kb-EscalationWriteupsKBKnowledgeBaseId': 'SV0hWoznYB', 'sim2real-kbcfn-0d981e2a1d3c-bedrock-kb-MetadataKBKnowledgeBaseId': 'HTAcBjbQOq'}

  Expected: after UPDATE_ROLLBACK_COMPLETE, ListExports under the KB stack's name prefix reports export `<kb-stack>-MetadataKBKnowledgeBaseId` with the SAME value the stack's MetadataKBKnowledgeBaseId Output carried before the update, and export `<kb-stack>-EscalationWriteupsKBKnowledgeBaseId` with the SAME value its EscalationWriteupsKBKnowledgeBaseId Output carried before the update.
  Actual: both exports carry different values — the ids minted for the g2/cohere replacement knowledge bases created during the cancelled forward pass.

  The concrete id strings ('fBNMCMnc9J', 'zjrxjBv9Tr', 'SV0hWoznYB', 'HTAcBjbQOq') and the generated stack token 'sim2real-kbcfn-0d981e2a1d3c-bedrock-kb' are this run's generated values filling the template slots; the requirement is that the two sides are equal, not any particular id. (The left-hand map is keyed by OutputKey and the right-hand by export name only because the check compares describe_stacks Outputs against ListExports names; the divergence is in the VALUES.)

No real-account or live-only value is involved: every id above is emulator-minted, and the assertion compares two emulator responses to each other.

## Steps to Reproduce
0. git_clone, then run_behavioral_suite and read the `### bedrock-agent__cloudformation__ssm` section of the failing-check list: bedrock-agent__cloudformation__ssm.kbcfn-embedding-model-replacement-rejected-while-kb-ids-imported is marked neither ALREADY FILED nor OWNED.

1. CreateStack a 'KB stack' with Capabilities=['CAPABILITY_IAM'] from a template holding an AWS::S3Vectors::VectorBucket, two AWS::S3Vectors::Index (DataType float32, Dimension 1024, DistanceMetric cosine), an AWS::IAM::Role assumable by bedrock.amazonaws.com, and two AWS::Bedrock::KnowledgeBase resources (KnowledgeBaseConfiguration.Type VECTOR, EmbeddingModelArn arn:aws:bedrock:<region>::foundation-model/amazon.titan-embed-text-v2:0, StorageConfiguration.Type S3_VECTORS), whose Outputs MetadataKBKnowledgeBaseId and EscalationWriteupsKBKnowledgeBaseId each Export under Fn::Sub '${AWS::StackName}-<OutputKey>'. Parameters: NamePrefix=<tok>, EmbeddingModelId=amazon.titan-embed-text-v2:0, Generation=g1. Wait for CREATE_COMPLETE.
2. Record `before` = the two Output values from DescribeStacks(StackName=<kb-stack>).
3. CreateStack an 'agent stack' whose single AWS::SSM::Parameter Value Fn::Joins both ids via Fn::ImportValue Fn::Sub '${KbStackName}-MetadataKBKnowledgeBaseId' and '${KbStackName}-EscalationWriteupsKBKnowledgeBaseId'. Wait for CREATE_COMPLETE.
4. UpdateStack the KB stack with the same template, Capabilities=['CAPABILITY_IAM'], Parameters NamePrefix=<tok>, EmbeddingModelId=cohere.embed-english-v3, Generation=g2 (a replacing change AWS refuses while the exports are imported). Poll DescribeStacks until terminal: UPDATE_ROLLBACK_COMPLETE (correct today).
5. Page ListExports and keep every Export whose Name starts with the KB stack name; build {Name: Value}.

Observed: both values differ from `before` — they are the ids of the g2 replacement knowledge bases.
Required: {f'{kb-stack}-{k}': v for k, v in before.items()} — the pre-update values, unchanged, under the same two export names.

6. As corroboration, GetKnowledgeBase on each id in `before` still reports status ACTIVE with embeddingModelArn arn:aws:bedrock:<region>::foundation-model/amazon.titan-embed-text-v2:0 — i.e. the original knowledge bases survive the rollback, but the exports no longer name them.

Re-replay: the check id in trace_ids.
