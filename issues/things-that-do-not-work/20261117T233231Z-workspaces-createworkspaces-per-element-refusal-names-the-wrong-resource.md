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
Issue-Commit: 5f9152346f0a34f19c016441be64bcf6cb300f64

# CreateWorkspaces per-element refusal names the wrong resource: an unresolvable BundleId is reported as ResourceNotFound.Directory

> _Note: this issue was identified against the code base at commit `5f9152346f0a34f19c016441be64bcf6cb300f64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWorkspaces

## Description
Cause 1 of 1: CreateWorkspaces reports a batch element's per-item failure against the DIRECTORY when the recording shows AWS reports it against the BUNDLE.

AWS: CreateWorkspaces models no missing-resource error and answers HTTP 200 (Smithy: CreateWorkspaces -> CreateWorkspacesResult, errors only InvalidParameterValuesException / ResourceLimitExceededException, both 400). A request element that cannot be satisfied comes back positionally in FailedRequests as a FailedCreateWorkspaceRequest carrying ErrorCode (target ErrorType, a free-form string: "The error code that is returned if the WorkSpace cannot be created") and ErrorMessage (target Description). WorkspaceRequest requires both DirectoryId (pattern ^(d-[0-9a-f]{8,63}$)|(wsd-[0-9a-z]{8,63}$)$) and BundleId (pattern ^wsb-[0-9a-z]{8,63}$); the model fixes no precedence between the two lookups and publishes no wording for ErrorMessage, so this is recorded behavior only. The recorded response for this element is ErrorCode 'ResourceNotFound.Bundle' with ErrorMessage 'The specified bundle could not be found in the specified region.' — the whole message is FIXED text for this error class, with NO variable part interpolated from the request or state (it names neither the bundle id nor the region). The requirement the recording establishes is the precedence: when an element's BundleId does not resolve, the element is refused as the BUNDLE failure, and the directory code is reserved for an element whose DirectoryId is the thing that does not resolve.

Emulator: the same element in the same batch comes back as ErrorCode 'ResourceNotFound.Directory' with ErrorMessage 'The specified directory could not be found in the specified region.' (status 200 -> 200, so only the per-item code and message diverge; every other element of the batch matched). The emulator resolves DirectoryId first and returns on the first failing lookup, so the bundle refusal is masked whenever the directory lookup also fails — the handler's own documentation states this tie-break was CHOSEN to follow the model's member declaration order (DirectoryId before BundleId) rather than taken from a recording, and the recording contradicts that choice.
Hypothesis: this element carries both an unresolvable directory and an unresolvable bundle, and AWS's precedence is the opposite of the one the emulator chose (bundle reported first).

Why: a client reads FailedRequests[i].ErrorCode to decide what to correct before it retries that element. Told 'ResourceNotFound.Directory', it goes looking at DescribeWorkspaceDirectories / RegisterWorkspaceDirectory and re-submits the same unusable BundleId, so the retry fails identically and forever; the real correction (pick a bundle DescribeWorkspaceBundles lists) is never signalled. Error-code branching and retry logic built against AWS therefore loops on a batch the real service tells it how to fix.

Fix: the per-element judgement in the WorkSpaces state store — the create path that returns the per-item (ErrorCode, ErrorMessage) pair consumed by the CreateWorkspaces handler. The bundle lookup must take precedence over the directory lookup when an element fails both, so ResourceNotFound.Bundle / 'The specified bundle could not be found in the specified region.' is what such an element reports; ResourceNotFound.Directory stays correct for an element whose directory alone is unresolvable. The existing per-item code and message constants are already correct and need no change — only the order in which the two refusals are produced.

Traces: workspaces-create-workspaces-batch-start-stop-reboot#task#3 (CreateWorkspaces, 200->200, $.FailedRequests[1].ErrorCode and $.FailedRequests[1].ErrorMessage). Relevant emulator code: services/service-com-amazonaws-workspaces-workspacesservice/src/state.rs lines 1731-1779 (create_workspace: directory check at 1761-1766 returns before the bundle check at 1774-1779; constants ERROR_CODE_BUNDLE_NOT_FOUND / MESSAGE_BUNDLE_NOT_FOUND at lines 171 and 192) and services/service-com-amazonaws-workspaces-workspacesservice/src/ops/create_workspaces.rs lines 178-212 (the handler copies that pair straight into the FailedRequests entry). No open issue covers this; the other open workspaces wire issue, 20261108T072651Z-workspaces-describeworkspacebundles-nexttoken-is-an-internal-positional-offset-not.md, is about DescribeWorkspaceBundles pagination tokens.

## Evidence
Fixture workspaces-create-workspaces-batch-start-stop-reboot, step task#3, operation CreateWorkspaces, service workspaces. Expected status 200, actual 200.

Expected (recorded AWS):
  $.FailedRequests[1].ErrorCode    = "ResourceNotFound.Bundle"
  $.FailedRequests[1].ErrorMessage = "The specified bundle could not be found in the specified region."

Actual (emulator):
  $.FailedRequests[1].ErrorCode    = "ResourceNotFound.Directory"
  $.FailedRequests[1].ErrorMessage = "The specified directory could not be found in the specified region."

Both messages are entirely fixed text for their error class — neither interpolates a request value, a resource id, a region or any state-derived value — so the divergence is purely which refusal the element is judged against. All other elements of the batch, and the surrounding Start/Stop/Reboot steps of the fixture, matched (cluster affected_count 1).

## Steps to Reproduce
1. Boot the emulator and replay fixture workspaces-create-workspaces-batch-start-stop-reboot up to step task#3.
2. That step calls CreateWorkspaces with a batch in which element index 1 names a BundleId that no stored/catalog bundle resolves (and a DirectoryId that is likewise not registered).
3. Read the 200 response's FailedRequests[1]: the emulator returns ErrorCode "ResourceNotFound.Directory" / ErrorMessage "The specified directory could not be found in the specified region.".
4. AWS's recorded response for the same request returns ErrorCode "ResourceNotFound.Bundle" / ErrorMessage "The specified bundle could not be found in the specified region." for that element.
5. Equivalent minimal check: submit a one-element CreateWorkspaces batch whose DirectoryId and BundleId are both unresolvable; the element must be reported as the bundle failure.
