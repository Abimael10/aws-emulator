Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3.change-set-include-nested-stacks-links-the-change-set-tree cloudformation__s3.nested-change-set-child-execution-status-unavailable
Issue-Key: cloudformation__s3.change-set-include-nested-stacks-links-the-change-set-tree|run#28|AssertionError|assert (listed[grand_cs].get("ParentChangeSetId"), listed[grand_cs].get("RootChangeSetId")) == (
Issue-Key: cloudformation__s3.nested-change-set-child-execution-status-unavailable|run#19|AssertionError|assert (child.get("ExecutionStatus"), child.get("StatusReason")) == (
Issue-Commit: e811b8b41259ffda648a98493ef74b5680a725e4

# cloudformation nested change sets: 2 net-new causes — ListChangeSets summaries omit ParentChangeSetId/RootChangeSetId, and a child change set carries no StatusReason explaining it is only executable from the root

> _Note: this issue was identified against the code base at commit `e811b8b41259ffda648a98493ef74b5680a725e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListChangeSets

## Description
Two distinct net-new root causes, both in the CloudFormation nested change-set path, both of which must be closed before this issue is submitted. The nested change-set TREE itself now gets built (the recursion that open issue 20261107T060626Z-cloudformation-s3-cloudformation-nested-stacks-no-nested-change-set-tree.md reported absent now produces child/grandchild change sets whose DescribeChangeSet linkage is correct), so these are the two remaining divergences left in that path; that issue does not cover them and both checks now fail past the point it describes.

Cause 1 of 2: ListChangeSets' ChangeSetSummary omits ParentChangeSetId and RootChangeSetId for a nested change set.
AWS: Smithy: ListChangeSets returns Summaries of com.amazonaws.cloudformation#ChangeSetSummary, which models ParentChangeSetId ("The parent change set ID.") and RootChangeSetId ("The root change set ID.") alongside StackId, StackName, ChangeSetId, ChangeSetName, ExecutionStatus, Status, StatusReason, CreationTime, Description, IncludeNestedStacks and ImportExistingResources. When a change set belongs to a nested change-set hierarchy, the summary ListChangeSets returns for the nested stack repeats the same linkage DescribeChangeSet reports for that change set: ParentChangeSetId is the change-set ARN of its immediate parent and RootChangeSetId is the change-set ARN of the root. Both are change-set ARNs taken from the hierarchy the request's stack belongs to, not fixed values.
Emulator: DescribeChangeSet on the grandchild change set reports the correct (StackId, ParentChangeSetId, RootChangeSetId), but ListChangeSets on the same nested stack returns a summary with NO ParentChangeSetId and NO RootChangeSetId member at all. The returned summary holds only StackId, StackName, ChangeSetId, ChangeSetName, ExecutionStatus, Status, CreationTime, IncludeNestedStacks and ImportExistingResources, so `summary.get("ParentChangeSetId")` and `summary.get("RootChangeSetId")` are both None.
Why: a client that enumerates a nested stack's change sets cannot tell which hierarchy each one belongs to, nor find the root change set that must be executed, without a second DescribeChangeSet per row. Console-style and pipeline tooling that walks the tree from ListChangeSets sees unparented change sets and cannot reconstruct the tree or clean it up.
Fix: the ListChangeSets handler's ChangeSetSummary serializer, which must project the nested hierarchy's parent/root change-set ids (already held for DescribeChangeSet) onto every summary row.
Traces: cloudformation__s3.change-set-include-nested-stacks-links-the-change-set-tree.

Cause 2 of 2: a nested child change set is correctly ExecutionStatus=UNAVAILABLE but carries no StatusReason saying why.
AWS: Smithy: DescribeChangeSetOutput.ExecutionStatus (enum) is documented "If the change set execution status is AVAILABLE, you can execute the change set. If you can't execute the change set, the status indicates why", and DescribeChangeSetOutput.StatusReason is "A description of the change set's status". For a change set created as part of a nested hierarchy (CreateChangeSet with IncludeNestedStacks=True), the root change set is ExecutionStatus=AVAILABLE while each nested child change set reaches Status=CREATE_COMPLETE with ExecutionStatus=UNAVAILABLE and StatusReason "Only executable from the root change set." — that message is fixed text with no variable part. A direct ExecuteChangeSet against the child id is then refused with InvalidChangeSetStatus and message "ChangeSet [<change-set-arn>] cannot be executed in its current execution status of [UNAVAILABLE]" (fixed text "ChangeSet [" / "] cannot be executed in its current execution status of [" / "]", with the requested change-set ARN and its current execution status as the variable parts).
Emulator: DescribeChangeSet on the child change set returns ExecutionStatus 'UNAVAILABLE' (correct) but StatusReason absent (None), so the response never says why the child cannot be executed. The check stops there, so whether the ExecuteChangeSet refusal above is also wrong is untested by this run.
Why: UNAVAILABLE is also the status of a change set that is merely still being created, so without the StatusReason a client cannot distinguish "wait and retry" from "this one is structurally non-executable, execute the root instead" and will poll a child change set forever.
Fix: the CreateChangeSet/DescribeChangeSet path for nested change sets — the child change-set record must carry the UNAVAILABLE StatusReason, and the DescribeChangeSet serializer must emit it.
Traces: cloudformation__s3.nested-change-set-child-execution-status-unavailable.

## Evidence
Cause 1 — cloudformation__s3.change-set-include-nested-stacks-links-the-change-set-tree [mismatch].
Expected: for the grandchild change set, ListChangeSets(StackName=<grandchild stack>) Summaries entry has (ParentChangeSetId, RootChangeSetId) == (<child change-set ARN>, <root change-set ARN>) — the same pair DescribeChangeSet already reports for that change set.
Actual: AssertionError at `assert (listed[grand_cs].get("ParentChangeSetId"), listed[grand_cs].get("RootChangeSetId")) == (child_cs, root_cs)`; the summary returned is {'StackId': ..., 'StackName': ..., 'ChangeSetId': ..., 'ChangeSetName': ..., 'ExecutionStatus': 'UNAVAILABLE', 'Status': 'CREATE_COMPLETE', 'CreationTime': ..., 'IncludeNestedStacks': True, 'ImportExistingResources': False} — neither ParentChangeSetId nor RootChangeSetId is present. The earlier assertions in the same check (root has no parent/root ids, child and grandchild DescribeChangeSet linkage, grandchild Changes == [('G2','Add')]) all passed.

Cause 2 — cloudformation__s3.nested-change-set-child-execution-status-unavailable [mismatch].
Expected: DescribeChangeSet(<child change-set id>) == (ExecutionStatus 'UNAVAILABLE', StatusReason 'Only executable from the root change set.').
Actual: AssertionError ('UNAVAILABLE', None) — the execution status is right, the StatusReason is missing. The preceding assertions (root change set Status CREATE_COMPLETE and ExecutionStatus AVAILABLE, child change set Status CREATE_COMPLETE) all passed.

## Steps to Reproduce
Cause 1: 1) CreateBucket; PutObject child.json (a single AWS::CloudFormation::Stack 'Grand' taking GrandUrl), grand-v1.json (handle G1) and grand-v2.json (handles G1 + G2). 2) CreateStack a parent whose 'Child' AWS::CloudFormation::Stack points at child.json with GrandUrl=grand-v1.json; wait CREATE_COMPLETE. 3) CreateChangeSet(StackName=<root>, ChangeSetType='UPDATE', IncludeNestedStacks=True, UsePreviousTemplate=True, Parameters=[GrandUrl=grand-v2.json]); wait CREATE_COMPLETE. 4) Walk the tree: root Changes[0].ResourceChange.ChangeSetId -> child change set; its 'Grand' ResourceChange.ChangeSetId -> grandchild change set. 5) Call ListChangeSets(StackName=<grandchild stack id>) and read the summary for the grandchild change set: ParentChangeSetId and RootChangeSetId are absent, although DescribeChangeSet on the same id reports both.

Cause 2: 1) CreateBucket; PutObject child-v1.json (a WaitConditionHandle only) and child-v2.json (the handle plus one more resource). 2) CreateStack a parent whose 'Child' AWS::CloudFormation::Stack points at child-v1.json; wait CREATE_COMPLETE. 3) CreateChangeSet(StackName=<root>, ChangeSetType='UPDATE', IncludeNestedStacks=True, UsePreviousTemplate=True, Parameters=[ChildUrl=child-v2.json]); wait CREATE_COMPLETE and confirm the root's ExecutionStatus is AVAILABLE. 4) Take the child change-set id from the root's 'Child' ResourceChange.ChangeSetId and DescribeChangeSet it: ExecutionStatus is 'UNAVAILABLE' but StatusReason is absent instead of 'Only executable from the root change set.'
