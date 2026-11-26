Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.update-association-status-invalid-document-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.update-association-status-invalid-document-rejected|run#16|AssertionError|assert describe_message == (
Issue-Commit: 8d1b1b750e0b1340c298c8ebe49c21e166bf4879

# ssm: DescribeAssociation's AssociationDoesNotExist message for a never-associated Name/InstanceId pair is emulator prose ("The specified association doesn't exist.") instead of AWS's documentName/instanceId template

> _Note: this issue was identified against the code base at commit `8d1b1b750e0b1340c298c8ebe49c21e166bf4879`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAssociation

## Description
One net-new root cause under the ssm heading.

Cause 1 of 1: DescribeAssociation refuses a legacy Name/InstanceId lookup that has no association with the correct code but a single emulator-authored sentence that names neither the document nor the instance and omits AWS's association-id hint. This check reaches the divergence on the "never associated" route — a freshly created Command document and a well-formed managed-node id that was never associated with it.

AWS: Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): DescribeAssociation takes Name (DocumentARN, @pattern ^[a-zA-Z0-9_\-.:/]{3,128}$), InstanceId (@pattern ^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$) and AssociationId, and declares AssociationDoesNotExist (errorKind client, httpError 400, awsQueryError code AssociationDoesNotExist). When the lookup is the legacy (Name, InstanceId) pair and no association exists for that pair, AWS's refusal message is the documentName/instanceId template: "Association not found for documentName <Name> and instanceId <InstanceId>. Try using the association-id parameter if you are looking for an association created using targets." Its FIXED text is "Association not found for documentName ", " and instanceId " and ". Try using the association-id parameter if you are looking for an association created using targets."; its VARIABLE parts are the Name and the InstanceId copied verbatim from the request. This is a distinct template from the AssociationId lookup form ("Association <AssociationId> does not exist."). The sibling UpdateAssociationStatus refusal on the same pair — code AssociationDoesNotExist with an EMPTY message — is served correctly by the emulator, so only the DescribeAssociation text diverges.

Emulator: the same DescribeAssociation(Name=<a real, Active Command document>, InstanceId=<a well-formed i-... never associated with it>) answers code AssociationDoesNotExist (correct) with Message "The specified association doesn't exist." — one fixed sentence that identifies no document, no instance, and gives no association-id hint.

Why: the error message is a requirement. AWS's text tells the caller WHICH (documentName, instanceId) pair has no association and that targets-created associations must be looked up by AssociationId instead. A client that confirms absence by matching the text, logs/reports the missing pair, or follows the hint to retry with AssociationId can do none of it; worse, the same opaque sentence is returned for both lookup forms, so a caller cannot tell which lookup failed.

Fix: the not-found refusal text in the SSM crate's describe_association handler and the association lookup that feeds it — select AWS's documentName/instanceId template (interpolating Name and InstanceId from the request) when the request carries Name+InstanceId, and the AssociationId template when it carries AssociationId.

Related open issues that do NOT cover this check: 20261122T161603Z-ssm-2-net-new-causes-a-maintenance-window.md describes the same message template under its Cause 2 but carries only the replay keys of ssm.update-association-status-nonexistent-association-rejected and ssm.update-association-status-transitions-and-nonexistent-rejected — not this check's; 20261113T123420Z-cloudformation-ec2-ssm-ssm-describeassociation-the-associationdoesnotexist-message-for-an-association.md covers only the AssociationId form of the message. Neither carries this check's replay key, so it is filed here and should be closed together with them.

Traces: ssm.update-association-status-invalid-document-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (478/639 passed, 74.8%; emulator up, no bootstrap signal). This check is neither marked ALREADY FILED nor OWNED by an open issue.

[mismatch] ssm.update-association-status-invalid-document-rejected
  File "check_ssm_update_association_status_invalid_document_rejected.py", line 79, in run
    assert describe_message == (...)
  AssertionError: The specified association doesn't exist.

Sequence (from the replayed source):
1. ssm CreateDocument(Name=<unique "sim2real-ssm-doc-...">, DocumentType="Command", Content={"schemaVersion":"2.2","mainSteps":[{"action":"aws:runShellScript","name":"noop","inputs":{"runCommand":["echo hi"]}}]}, Tags=[...]); GetDocument reports Status Active — positive control passes, the document is real and resolvable.
2. ssm UpdateAssociationStatus(Name=<that document>, InstanceId="i-<17 hex>" never associated with it, AssociationStatus={Date, Name:"Success", Message:...}) -> code AssociationDoesNotExist with an EMPTY Message. Both assertions pass: the emulator matches AWS here.
3. ssm DescribeAssociation(Name=<same document>, InstanceId=<same instance id>) -> code AssociationDoesNotExist (asserted and passing).
   Expected Message: "Association not found for documentName <doc_name> and instanceId <instance_id>. Try using the association-id parameter if you are looking for an association created using targets."
   Actual Message:   "The specified association doesn't exist."

So the HTTP status (400) and the error code (AssociationDoesNotExist) are right; only the message diverges, and it diverges in its fixed text — it is not a per-run value difference. The document name and instance id in the expected message are the ones from the request (the check generates them per run); the surrounding sentence is the requirement.

Model evidence: DescribeAssociation inputs Name (DocumentARN pattern) / InstanceId (i-|mi- pattern) / AssociationId; error AssociationDoesNotExist (client, httpError 400, awsQueryError AssociationDoesNotExist httpResponseCode 404).

## Steps to Reproduce
1. git clone the repo, build and boot the emulator from this checkout.
2. ssm create-document --name <unique-doc> --document-type Command --content '{"schemaVersion":"2.2","description":"no-op command document","mainSteps":[{"action":"aws:runShellScript","name":"noop","inputs":{"runCommand":["echo hi"]}}]}'; confirm ssm get-document --name <unique-doc> reports Status Active.
3. Pick a well-formed managed-node id that was never associated with it, e.g. i-<17 hex chars>.
4. ssm update-association-status --name <unique-doc> --instance-id <that id> --association-status Date=<now>,Name=Success,Message=... : refused AssociationDoesNotExist with an empty message (matches AWS).
5. ssm describe-association --name <unique-doc> --instance-id <that id>: the emulator answers code AssociationDoesNotExist with Message "The specified association doesn't exist."; AWS answers the same code with "Association not found for documentName <unique-doc> and instanceId <that id>. Try using the association-id parameter if you are looking for an association created using targets.".
6. Re-replay ssm.update-association-status-invalid-document-rejected to confirm the fix (and re-replay the two checks named in 20261122T161603Z-ssm-2-net-new-causes-a-maintenance-window.md Cause 2, which exercise the deleted-association route of the same template).
