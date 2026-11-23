Issue-Source: trace-sweep-divergence
Issue-Trace: iot-account-configuration-inspection
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-account-configuration-inspection
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|$.message
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|status
Issue-Commit: 7f611a950cc110a85e462673301ed87b86ee66e7

# iot DescribeManagedJobTemplate refuses an unpublished templateName with 400 InvalidRequestException instead of 404 ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `7f611a950cc110a85e462673301ed87b86ee66e7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeManagedJobTemplate

## Description
Cause 1 of 1: a well-formed templateName that the AWS-published managed-job-template catalog does not hold is refused with the wrong error class, status and sentence.

AWS: Smithy: DescribeManagedJobTemplate GET /managed-job-templates/{templateName}, success code 200; input templateName is @required, @httpLabel, @length 1..64; the operation models InternalServerException (server, 500), InvalidRequestException (client, 400), ResourceNotFoundException (client, 404) and ThrottlingException (client, 400). For a well-formed templateName that names no published managed template, the recording shows AWS answering HTTP 404 with header x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/' and body message 'ManagedJobTemplate with name: <templateName> not found'. The fixed text of that message is 'ManagedJobTemplate with name: ' and ' not found'; its single variable part is the templateName echoed verbatim from the request path (the recording happens to use the literal name the request sent). AWS therefore treats an unknown managed-template name as a missing resource, not as an invalid request.

Emulator: answers HTTP 400 with x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/' and message 'Invalid ManagedJobTemplate name: <templateName>' — wrong status class (4xx client error, but 400 rather than 404), wrong modeled error shape, wrong errortype header value and wrong sentence. Replay reported: status expected 404, got 400; $.message expected 'ManagedJobTemplate with name: AWS-Does-Not-Exist not found', got 'Invalid ManagedJobTemplate name: AWS-Does-Not-Exist'; $headers.x-amzn-errortype expected 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', got 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/'.

Why: a client that catches the modeled ResourceNotFoundException (or that branches on HTTP 404) to discover whether a managed template exists before creating a job from it never reaches that branch — it gets InvalidRequestException/400, which AWS reserves for a malformed request and which most SDK retry/validation layers classify as a caller bug that must not be retried or recovered. Probing the catalog for a template name therefore fails closed, and both status-code and error-message assertions break.

Note: this is the inverse of a RESOLVED issue, 20261122T022503Z-iot-describemanagedjobtemplate-diverges-on-2-causes-the-published.md, whose Cause 2 asserted the opposite mapping (400 InvalidRequestException for an unknown name) and whose fix introduced the current behavior. A resolved issue does not cover this failure; the corpus recording is the ground truth and it requires 404 ResourceNotFoundException. The unpublished-VERSION refusal in the same handler ('Invalid ManagedJobTemplate version: <v>') is NOT part of this finding and must stay InvalidRequestException/400.

Fix: the not-found branch of the DescribeManagedJobTemplate handler and the refusal sentence it draws from the catalog state — map a templateName the published catalog does not hold to ResourceNotFoundException (404) with AWS's 'ManagedJobTemplate with name: <templateName> not found' sentence, keeping the unpublished-version branch on InvalidRequestException. Also ensure the errortype header carries the service's qualified shape form 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/'.

Traces: iot-account-configuration-inspection#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 169 fixtures, 1120 steps, 848 passed, score 75.7%, 41 divergence clusters.

Fixture iot-account-configuration-inspection, step task#4, operation DescribeManagedJobTemplate, verdict mismatch, affected_count 1, divergence_key 'service=iot|DescribeManagedJobTemplate|status=404->400|keys=$.message,$headers.x-amzn-errortype,status':
  status: expected 404, got 400
  $.message: expected 'ManagedJobTemplate with name: AWS-Does-Not-Exist not found', got 'Invalid ManagedJobTemplate name: AWS-Does-Not-Exist'
  $headers.x-amzn-errortype: expected 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', got 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/'

Model evidence (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json): DescribeManagedJobTemplate GET /managed-job-templates/{templateName}, code 200; input templateName (@required, @httpLabel, @length 1..64), templateVersion (@httpQuery, @pattern ^[1-9]+.[0-9]+$); errors InternalServerException(500), InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400) — ResourceNotFoundException is modeled on this operation and is the shape AWS uses here.

Code: services/service-com-amazonaws-iot-awsiotservice/src/ops/describe_managed_job_template.rs lines 186-202 returns DescribeManagedJobTemplateErrorModeled::InvalidRequestException with crate::state::invalid_managed_job_template_name_message(&input.template_name) when store.managed_job_templates.template(..) yields None.

## Steps to Reproduce
Boot the emulator from this checkout and replay fixture iot-account-configuration-inspection, step task#4, or exercise the call directly against the iot endpoint:

GET /managed-job-templates/<a well-formed 1..64-char name the published managed catalog does not hold> (e.g. aws iot describe-managed-job-template --template-name AWS-Does-Not-Exist), with no templateVersion query parameter.

Expected: HTTP 404, header x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', body message 'ManagedJobTemplate with name: <that name> not found'.
Observed: HTTP 400, header x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', body message 'Invalid ManagedJobTemplate name: <that name>'.

Regression guard: GET /managed-job-templates/AWS-Download-File?templateVersion=<a version that template does not publish> must still answer 400 InvalidRequestException with 'Invalid ManagedJobTemplate version: <v>'.
