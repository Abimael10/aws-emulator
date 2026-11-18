Issue-Source: trace-sweep-divergence
Issue-Trace: workspaces-create-workspaces-batch-start-stop-reboot
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: workspaces-create-workspaces-batch-start-stop-reboot
Issue-Shape-Id: com.amazonaws.workspaces#WorkspacesService
Issue-Key: workspaces|CreateWorkspaces|2xx->2xx|$.FailedRequests[*].ErrorCode
Issue-Key: workspaces|CreateWorkspaces|2xx->2xx|$.FailedRequests[*].ErrorMessage
Issue-Commit: 3439b20705c2d76fe086808e512030e6212952f5

# CreateWorkspaces reports an unresolvable bundle as ResourceNotFound.Directory, so the per-item failure names the wrong resource

> _Note: this issue was identified against the code base at commit `3439b20705c2d76fe086808e512030e6212952f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWorkspaces

## Description
One cause.

Cause 1 of 1: a batch element whose BundleId resolves to no stored bundle is reported in FailedRequests with the DIRECTORY error code and the directory error message instead of the bundle ones. Fixture workspaces-create-workspaces-batch-start-stop-reboot, step task#3.

AWS: CreateWorkspaces judges every WorkspaceRequest in the batch on its own and reports an element it cannot satisfy inside a 200 response as a FailedRequests entry (Smithy: CreateWorkspaces -> CreateWorkspacesResult.FailedRequests : FailedCreateWorkspaceRequests; FailedCreateWorkspaceRequest.ErrorCode targets ErrorType, "The error code that is returned if the WorkSpace cannot be created", a free-form string with no enum, and ErrorMessage targets Description; the operation models only InvalidParameterValuesException and ResourceLimitExceededException as top-level errors, so a missing resource is never a non-200). WorkspaceRequest requires both DirectoryId (pattern ^(d-[0-9a-f]{8,63}$)|(wsd-[0-9a-z]{8,63}$)$) and BundleId (pattern ^wsb-[0-9a-z]{8,63}$). The model gives no vocabulary for ErrorCode and documents no precedence between the two lookups, so this is recorded behavior only: for this element AWS answered ErrorCode 'ResourceNotFound.Bundle' with ErrorMessage 'The specified bundle could not be found in the specified region.' — i.e. an element whose bundle does not resolve is reported against the BUNDLE. The message is entirely fixed text for this error class: it interpolates no request or state value (no bundle id, no region literal appears in it), and the sibling directory class is the same sentence with the resource word 'directory' and no interpolation either. Hypothesis: AWS resolves the bundle before, or in preference to, the directory, so for an element where neither resolves the caller is told about the bundle.

Emulator: the same step answered HTTP 200 with the same FailedRequests shape but FailedRequests[1].ErrorCode = 'ResourceNotFound.Directory' and FailedRequests[1].ErrorMessage = 'The specified directory could not be found in the specified region.' The per-element judgement in the WorkSpaces store checks the directory registry first and returns the directory refusal immediately, so the bundle lookup is never reached for that element (services/service-com-amazonaws-workspaces-workspacesservice/src/state.rs: the directory check and its ERROR_CODE_DIRECTORY_NOT_FOUND return precede the bundle check and its ERROR_CODE_BUNDLE_NOT_FOUND return in ComAmazonawsWorkspacesWorkspacesserviceState::create_workspace; the doc comment above that function states the precedence was CHOSEN from the model's member order — DirectoryId declared before BundleId — not taken from a recording, which is exactly the choice the recording contradicts).

Why: the per-item ErrorCode is the only machine-readable signal a client has for a partially failed batch, and clients branch on it — retry the element with a different BundleId (re-resolving the catalog via DescribeWorkspaceBundles) versus register/repair the directory (DescribeWorkspaceDirectories / RegisterWorkspaceDirectory). Being told 'ResourceNotFound.Directory' for a bundle problem sends the client down the directory remediation path, where it finds a directory that is present and healthy, and the retry re-submits the same unresolvable BundleId forever. The ErrorMessage surfaced to an operator names the wrong resource for the same reason.

Fix: the per-element resolution order in the CreateWorkspaces element judgement in the service's state (ComAmazonawsWorkspacesWorkspacesserviceState::create_workspace) — judge the BundleId against the bundle store before (or in preference to) the DirectoryId against the directory registry, so an element with an unresolvable bundle is reported as ResourceNotFound.Bundle / 'The specified bundle could not be found in the specified region.' whatever its DirectoryId resolves to, and ResourceNotFound.Directory / 'The specified directory could not be found in the specified region.' is reported only when the bundle resolves and the directory does not. The CreateWorkspaces handler itself (which echoes the refused request and lifts the (code, message) pair into FailedRequests) needs no change, and the response must stay a 200.

Traces: workspaces-create-workspaces-batch-start-stop-reboot#task#3 ($.FailedRequests[1].ErrorCode, $.FailedRequests[1].ErrorMessage).

Not covered by any open issue: the only open workspaces wire issue, 20261108T072651Z-workspaces-describeworkspacebundles-nexttoken-is-an-internal-positional-offset-not.md, is about DescribeWorkspaceBundles pagination tokens and owns the other divergence in this corpus.

## Evidence
Fixture workspaces-create-workspaces-batch-start-stop-reboot, step task#3, CreateWorkspaces, expected HTTP 200 / actual HTTP 200:
  $.FailedRequests[1].ErrorCode: expected 'ResourceNotFound.Bundle', got 'ResourceNotFound.Directory'
  $.FailedRequests[1].ErrorMessage: expected 'The specified bundle could not be found in the specified region.', got 'The specified directory could not be found in the specified region.'
Every other member of the response, including FailedRequests[1].WorkspaceRequest and the rest of the batch's FailedRequests/PendingRequests entries, matched — only the resource the per-item failure names differs.
Message anatomy: both sentences are 100% fixed text for their error class; neither carries a value from the request or from state, so the requirement is the class, not the literal.

## Steps to Reproduce
1. Replay the wire corpus against the emulator (replay_wire_corpus).
2. Fixture workspaces-create-workspaces-batch-start-stop-reboot, step task#3, calls CreateWorkspaces with a batch of WorkspaceRequest elements in which the element at index 1 cannot be satisfied because its BundleId names no bundle in the store.
3. Read FailedRequests[1] of the 200 response: the emulator reports ErrorCode 'ResourceNotFound.Directory' / ErrorMessage 'The specified directory could not be found in the specified region.' where the recording has 'ResourceNotFound.Bundle' / 'The specified bundle could not be found in the specified region.'
