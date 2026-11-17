Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: resiliencehubv2.delete-service-cascades-to-child-resources
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2|ListInputSources|ResourceNotFoundException
Issue-Commit: a4215905cca27c128686d10d799e4c7536c2e42a

# resiliencehubv2 DeleteService destroys the service's input sources: ListInputSources/DeleteInputSource on the deleted service ARN answer ResourceNotFoundException instead of still serving the surviving input sources

> _Note: this issue was identified against the code base at commit `a4215905cca27c128686d10d799e4c7536c2e42a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteService

## Description
Cause 1 of 1: an input source outlives the service it was created under, but the emulator stores input sources inside the service record and drops them with it, so every read of them through the now-dead service ARN is refused.

AWS: recorded behavior only (the Smithy model documents DeleteService as only "Deletes a service." and says nothing about cascade scope). The recording shows that after a successful DeleteService on a service that has a service function, an assertion and an input source: GetService, ListServiceFunctions and ListAssertions on that ARN are all refused with HTTP 404 ResourceNotFoundException and message `Service not found: <serviceId>` (fixed text `Service not found: ` plus the id segment of the serviceArn from the request), i.e. the function and assertion children are gone with the parent; but ListInputSources on the SAME dead serviceArn still succeeds with HTTP 200 and still lists the input source's inputSourceId in inputSourceSummaries, and a subsequent DeleteInputSource(serviceArn=<dead arn>, inputSourceId=...) still succeeds with HTTP 200 and removes it, after which ListInputSources on the dead ARN returns a list no longer containing that id. Input sources are the documented exception to the delete cascade: they survive their parent and stay addressable (and deletable) through the pre-delete ARN. Smithy: ListInputSources is GET /v2/list-input-sources with required serviceArn and required output member inputSourceSummaries; DeleteInputSource is POST /v2/delete-input-source with required serviceArn + inputSourceId; both model ResourceNotFoundException (404).

Emulator: DeleteService removes the whole service record from the store (`store.services.remove(&input.service_arn)` — services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/delete_service.rs), and the input sources are held inside that record, so they are destroyed with it. ListInputSources then looks the ARN up in `store.services` and, finding nothing, returns ResourceNotFoundException — the replay fails with `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the ListInputSources operation: Service not found: service/<uuid>` at the post-delete read, where AWS answers 200 with the surviving summary. DeleteInputSource on the dead ARN is unreachable for the same reason.

Why: a client that created input sources under a service loses them silently when the service is deleted. It cannot enumerate them and cannot delete them, so an orphan-cleanup path that walks ListInputSources on the pre-delete ARN (exactly what a teardown does) breaks with a 404, and code that treats the 404 as "already gone" leaks the resource. Conversely, a client written against the emulator would believe input sources are reaped with their service and omit the explicit DeleteInputSource that real AWS requires.

Fix: the input-source records must not be owned by the service record's lifetime — hold them in the service store keyed by serviceArn (or move them to a store-level collection on delete) so DeleteService drops only the service, its functions and its assertions, and so ListInputSources and DeleteInputSource resolve a serviceArn whose service is gone but whose input sources remain. The service-existence 404 guard in ListInputSources/DeleteInputSource must not gate on a live service when input sources for that ARN still exist; a serviceArn that never had any input sources keeps the current 404.

Traces: resiliencehubv2.delete-service-cascades-to-child-resources (fails at the post-delete ListInputSources on the dead ARN; the pre-delete listings and the GetService/ListServiceFunctions/ListAssertions 404 assertions all pass).

## Evidence
Failing check: resiliencehubv2.delete-service-cascades-to-child-resources [error]

Expected (AWS, per the recording), after DeleteService(serviceArn=A) succeeds:
  - GetService(A)            -> 404 ResourceNotFoundException "Service not found: <serviceId>"  (emulator matches)
  - ListServiceFunctions(A)  -> 404 ResourceNotFoundException "Service not found: <serviceId>"  (emulator matches)
  - ListAssertions(A)        -> 404 ResourceNotFoundException "Service not found: <serviceId>"  (emulator matches)
  - ListInputSources(A)      -> 200, inputSourceSummaries still contains the inputSourceId created before the delete
  - DeleteInputSource(A, id) -> 200
  - ListInputSources(A)      -> 200, inputSourceSummaries no longer contains that id

Actual (emulator):
  - ListInputSources(A) -> HTTP 404 ResourceNotFoundException, message "Service not found: service/7f48c4b3-aecb-8d22-bd30-8337e989eca4"
    (the uuid is this run's value; the requirement is the fixed text "Service not found: " plus the request ARN's id segment — that wording is correct for a truly unknown service, it is simply the wrong outcome here)
  botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the ListInputSources operation: Service not found: service/7f48c4b3-aecb-8d22-bd30-8337e989eca4

Source: delete_service.rs:152-167 removes the service record (its comment states "deleting a service cascades to the child resources it holds"); list_input_sources.rs:216-224 refuses any serviceArn absent from `store.services`.

## Steps to Reproduce
1. iam.CreateRole with a resiliencehub.amazonaws.com trust policy; use its name as permissionModel.invokerRoleName.
2. resiliencehubv2.CreateService(name=<unique>, regions=[<region>], permissionModel={invokerRoleName}) -> serviceArn A.
3. CreateServiceFunction(serviceArn=A, name="checkout flow", criticality="PRIMARY").
4. CreateAssertion(serviceArn=A, text="Service must survive an AZ outage.").
5. CreateInputSource(serviceArn=A, resourceConfiguration={resourceTags:[{key:"app",values:["checkout"]}]}) -> inputSourceId S.
6. Confirm ListServiceFunctions(A), ListAssertions(A) and ListInputSources(A) each show their child.
7. DeleteService(serviceArn=A) -> 200.
8. ListInputSources(serviceArn=A): AWS returns 200 with S still listed; the emulator returns 404 ResourceNotFoundException "Service not found: <id of A>".
9. DeleteInputSource(serviceArn=A, inputSourceId=S): AWS returns 200 and the following ListInputSources(A) no longer lists S; the emulator cannot reach either call.
