Issue-Source: trace-sweep-divergence
Issue-Trace: glue-workflow-trigger-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-workflow-trigger-lifecycle glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetWorkflow|2xx->2xx|$.Workflow.Graph.Edges[*].DestinationId
Issue-Key: glue|GetWorkflow|2xx->2xx|$.Workflow.Graph.Edges[*].SourceId
Issue-Key: glue|GetWorkflow|2xx->2xx|$.Workflow.Graph.Nodes[*].UniqueId
Issue-Key: glue|GetJob|2xx->2xx|$.Job.Command.PythonVersion
Issue-Commit: cda032ca99efd252a540b47900271345a944d5a8

# Glue workflow graph ids and job PythonVersion default diverge from AWS

> _Note: this issue was identified against the code base at commit `cda032ca99efd252a540b47900271345a944d5a8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetWorkflow

## Description
Two Glue read paths answer 200 but report values AWS does not: the workflow graph's node/edge identifiers are minted as random v4 UUIDs instead of the identifier form AWS assigns, and a job created without Command.PythonVersion reads back with the wrong defaulted Python version. Both are value defects inside an otherwise correct response body, so a client sees a well-formed response that does not describe what AWS would have returned.

Cause 1 of 2: GetWorkflow's Graph node and edge identifiers are random UUIDs, not AWS-shaped node ids.
AWS: Smithy: GetWorkflow output Workflow.Graph (WorkflowGraph) with Nodes (NodeList) and Edges (EdgeList); Node.UniqueId targets NameString (@length min 1 max 255), documented as 'The unique Id assigned to the node within the workflow'; Edge.SourceId and Edge.DestinationId target the same NameString and are documented as 'The unique of the node within the workflow where the edge starts/ends', i.e. they must be the UniqueId values of the graph's own nodes. The model constrains only length and the NameString character class, so the identifier FORM is recorded behavior only: the fixture asserts the shape class of these four generated ids rather than their literal values (they are per-environment values), and AWS's recorded ids do not have the form the emulator emits. Hypothesis: AWS assigns each node an id derived from the component the node stands for (its type and name) rather than a fresh random identifier, which is why the recorded ids are reproducible for the same workflow.
Emulator: answers 200 with Workflow.Graph.Nodes[*].UniqueId and the matching Workflow.Graph.Edges[*].SourceId/DestinationId set to bare RFC-4122 v4 UUIDs (in this replay 'bdec6019-c6ac-45a6-b0ce-29dae29b52d2' and '0252f030-31d4-4386-ab62-810e490b0649' — per-run values, not a requirement), which fails the recorded shape for all four members. The emulator is at least self-consistent inside one response: the edge endpoints do reference the ids it gave the nodes.
Why: a client walking a workflow graph joins Edges to Nodes by these ids and, more importantly, correlates a node id with the trigger/job/crawler it names and with ids it has seen before (console deep links, graph diffing, stored references). An id in a form AWS never emits breaks any consumer that parses or re-resolves it, and an id that is not derived from the component cannot be matched back to the workflow component across calls or recreations.
Fix: the Glue workflow state's node-id assignment and the GetWorkflow graph serializer (the same generator must feed the Edge SourceId/DestinationId it already keeps consistent).
Traces: glue-workflow-trigger-lifecycle#task#7 ($.Workflow.Graph.Nodes[0].UniqueId, $.Workflow.Graph.Nodes[1].UniqueId, $.Workflow.Graph.Edges[0].SourceId, $.Workflow.Graph.Edges[0].DestinationId).

Cause 2 of 2: GetJob defaults Command.PythonVersion to '3' where AWS reports '2'.
AWS: Smithy: JobCommand.PythonVersion targets PythonVersionString with @pattern ^([2-3]|3[.]9)$ and the doc 'The Python version being used to run a Python shell job. Allowed values are 2 or 3'; the member carries no @default in the model. Recorded behavior: when CreateJob supplies a Command without PythonVersion, AWS stores and reads back PythonVersion '2' — the effective default is the lower version, not the newer one.
Emulator: answers 200 with $.Job.Command.PythonVersion '3' for a job created with no PythonVersion.
Why: a client that reads the job back to learn its effective runtime, asserts on it, or round-trips the job definition into UpdateJob/IaC sees a different Python runtime than AWS reports, which both mis-describes the job and produces a spurious diff on every reconcile.
Fix: the CreateJob/UpdateJob job-command default in Glue job state (the value materialised when Command.PythonVersion is absent), read back unchanged by the GetJob serializer.
Traces: glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found#task#3 ($.Job.Command.PythonVersion).

Related open issue: 20261117T133709Z-glue-read-paths-omit-or-misreport-members-aws.md describes the same PythonVersion default in its prose, but its replay pointer carries only the GetJob $.Job.CreatedOn / $.Job.LastModifiedOn keys, so this divergence is not covered by it and came back unowned in the replay; a fixer can close both together. No open issue mentions the workflow-graph identifiers.

## Evidence
1. glue-workflow-trigger-lifecycle#task#7 GetWorkflow — expected 200, got 200, but $.Workflow.Graph.Nodes[0].UniqueId, $.Workflow.Graph.Nodes[1].UniqueId, $.Workflow.Graph.Edges[0].SourceId and $.Workflow.Graph.Edges[0].DestinationId all report value_mismatch against the recording's shape-asserted ids: the emulator returned v4 UUIDs ('bdec6019-c6ac-45a6-b0ce-29dae29b52d2' for Nodes[0]/Edges[0].SourceId, '0252f030-31d4-4386-ab62-810e490b0649' for Nodes[1]/Edges[0].DestinationId — per-run values shown only to identify the form). Smithy constrains these to NameString, length 1..255.
2. glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found#task#3 GetJob — expected 200, got 200, but $.Job.Command.PythonVersion value_mismatch: expected '2' (AWS's default for a Command submitted without PythonVersion), got '3'.
Both clusters were reported by replay_wire_corpus with verdict 'mismatch', empty stub_path and empty owned_by (divergence indexes 28 and 20 of 28), against trace set simparity-wire-aws-v1.

## Steps to Reproduce
1. Call replay_wire_corpus() against the emulator built from this checkout.
2. Cause 1: CreateWorkflow, then create two triggers in it so the graph has two nodes and one edge, then GetWorkflow with IncludeGraph — every Workflow.Graph.Nodes[*].UniqueId and the Edges[*].SourceId/DestinationId that reference them come back as random v4 UUIDs instead of AWS's node-id form (replay fixture glue-workflow-trigger-lifecycle, step task#7).
3. Cause 2: CreateJob with a Command that sets Name/ScriptLocation but omits PythonVersion, then GetJob on that name — $.Job.Command.PythonVersion is '3' where AWS reports '2' (replay fixture glue-job-etl-unset-version-defaults-tags-provider-update-max-retries-and-not-found, step task#3).
