Issue-Source: trace-sweep-divergence
Issue-Trace: workspaces-create-and-migrate-workspace-to-new-bundle
Issue-Step: setup#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: workspaces-create-and-migrate-workspace-to-new-bundle workspaces-create-and-rebuild-workspace workspaces-create-and-restore-workspace workspaces-create-primary-and-standby-workspace-dr workspaces-create-workspaces-batch-start-stop-reboot workspaces-operate-on-terminated-workspace-errors
Issue-Shape-Id: com.amazonaws.workspaces#WorkspacesService
Issue-Key: workspaces|CreateWorkspaces|2xx->2xx|$.FailedRequests[*].ErrorCode
Issue-Key: workspaces|CreateWorkspaces|2xx->2xx|$.FailedRequests[*].ErrorMessage
Issue-Commit: ba6912b745241f4a8c68b84730217fa4109e44cb

# CreateWorkspaces per-element refusal names the bundle instead of the unresolvable directory

> _Note: this issue was identified against the code base at commit `ba6912b745241f4a8c68b84730217fa4109e44cb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWorkspaces

## Description
Cause 1 of 1: CreateWorkspaces reports the WRONG resource in its per-element FailedRequests entry when the element's DirectoryId names no registered WorkSpaces directory.

AWS: CreateWorkspaces is a batch operation that models no missing-resource error (Smithy: CreateWorkspaces input Workspaces -> WorkspaceRequestList @length 1..25 required; output FailedRequests -> FailedCreateWorkspaceRequests / PendingRequests -> WorkspaceList; modeled errors only InvalidParameterValuesException (400) and ResourceLimitExceededException (400)). A refused element is therefore reported inside a 200 as a FailedCreateWorkspaceRequest whose ErrorCode targets com.amazonaws.workspaces#ErrorType ("The error code that is returned if the WorkSpace cannot be created") - a free-form per-item code vocabulary, no enum, so the model does not enumerate the values. Recorded behavior only for which resource wins: for an element submitted against a DirectoryId that is not registered with WorkSpaces, the recorded AWS response is HTTP 200 with FailedRequests[0].ErrorCode = "ResourceNotFound.Directory" and FailedRequests[0].ErrorMessage = "The specified directory could not be found in the specified region." The message is entirely FIXED text for this error class - it carries no value taken from the request or from state (no directory id, no bundle id, no region literal), so it is reproduced verbatim; the code is the dotted <Condition>.<Resource> form naming the DIRECTORY as the resource that failed to resolve.

Emulator: the same step answers HTTP 200 but with FailedRequests[0].ErrorCode = "ResourceNotFound.Bundle" and FailedRequests[0].ErrorMessage = "The specified bundle could not be found in the specified region." The per-element resolution order is inverted: the handler judges the element's BundleId against the seeded bundle catalog BEFORE it judges the DirectoryId against the directory registry, and returns on the first miss, so an element whose bundle also fails to resolve is always attributed to the bundle and the directory refusal is never reported. The emulator's own comment states that precedence as the AWS behavior ("The bundle is resolved in PREFERENCE to the directory, which is what AWS answers for an element where neither resolves"); the recording contradicts it - AWS attributes the element to the directory. Hypothesis: the second half of the same defect may be that the recorded request's BundleId names an Amazon-provided catalog bundle that AWS resolves and the emulator does not stock, because the emulator seeds its catalog with synthetic ids derived from the Compute enum (wsb-amzn<compute>) rather than the Amazon-provided bundle ids the recording uses; either way the directory check must be evaluated before the bundle check for this element to be reported as AWS reports it.

Why: a client branches on FailedRequests[].ErrorCode to decide what to repair before retrying the batch. Told "ResourceNotFound.Bundle" it goes looking for a bundle (DescribeWorkspaceBundles, CreateWorkspaceBundle) while the actual fault is an unregistered directory, so the retry fails identically forever; provisioning tooling that keys remediation off the per-item code (register the directory via RegisterWorkspaceDirectory) never fires, and the paired ErrorMessage shown to an operator points at the wrong resource.

Fix: the per-element refusal ordering in the CreateWorkspaces state transition - the directory-registry lookup must be evaluated before the bundle-catalog lookup, so an element whose directory does not resolve is reported with ERROR_CODE_DIRECTORY_NOT_FOUND / MESSAGE_DIRECTORY_NOT_FOUND whatever its BundleId resolves to, and the bundle code is reported only for an element whose directory resolved. The Amazon-provided bundle catalog the create resolves against belongs in the same change if it does not stock the ids AWS serves.

Traces: fixture workspaces-create-and-migrate-workspace-to-new-bundle step setup#3 (representative), and the same cluster in workspaces-create-and-rebuild-workspace#task#3, workspaces-create-and-restore-workspace#setup#3, workspaces-create-primary-and-standby-workspace-dr#setup#3, workspaces-create-workspaces-batch-start-stop-reboot#task#3, workspaces-operate-on-terminated-workspace-errors#setup#3. Implementation: services/service-com-amazonaws-workspaces-workspacesservice/src/state.rs lines 1726-1778 (the ordering comment and the two lookups), and src/ops/create_workspaces.rs lines 178-212 (where the per-item code is echoed); the synthetic catalog seed is state.rs lines 2529-2555.

## Evidence
Step workspaces-create-and-migrate-workspace-to-new-bundle#setup#3, operation CreateWorkspaces, verdict mismatch, expected status 200 / actual status 200.

Expected (recorded AWS):
  $.FailedRequests[0].ErrorCode    = "ResourceNotFound.Directory"
  $.FailedRequests[0].ErrorMessage = "The specified directory could not be found in the specified region."

Actual (emulator):
  $.FailedRequests[0].ErrorCode    = "ResourceNotFound.Bundle"
  $.FailedRequests[0].ErrorMessage = "The specified bundle could not be found in the specified region."

Divergence key: service=workspaces|CreateWorkspaces|status=200->200|keys=$.FailedRequests[0].ErrorCode,$.FailedRequests[0].ErrorMessage
Cluster size: 6 steps across 6 fixtures (affected_count 6). Both diverging values are the FIXED text of their error class; neither recorded message interpolates any request or state value.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture workspaces-create-and-migrate-workspace-to-new-bundle, step setup#3: a CreateWorkspaces call whose single WorkspaceRequest names a DirectoryId that has not been registered with WorkSpaces (no RegisterWorkspaceDirectory preceded it).
3. Read the 200 response body: FailedRequests[0].ErrorCode is "ResourceNotFound.Bundle" with ErrorMessage "The specified bundle could not be found in the specified region.", where the recording carries "ResourceNotFound.Directory" / "The specified directory could not be found in the specified region."
4. The same mismatch reproduces at workspaces-create-and-rebuild-workspace#task#3, workspaces-create-and-restore-workspace#setup#3, workspaces-create-primary-and-standby-workspace-dr#setup#3, workspaces-create-workspaces-batch-start-stop-reboot#task#3 and workspaces-operate-on-terminated-workspace-errors#setup#3.
