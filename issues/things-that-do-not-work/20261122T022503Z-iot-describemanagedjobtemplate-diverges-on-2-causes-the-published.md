Issue-Source: trace-sweep-divergence
Issue-Trace: iot-catalog-describe-managed-job-template
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-catalog-describe-managed-job-template iot-describe-managed-job-template-not-found-error
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="downloadUrl"].example
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|$.message
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|status
Issue-Commit: 0b600564062992274bd01b2b8270b52f52f6e224

# iot DescribeManagedJobTemplate diverges on 2 causes: the published managed-template catalog drops a documentParameter example and an unknown template name is refused with the wrong error class

> _Note: this issue was identified against the code base at commit `0b600564062992274bd01b2b8270b52f52f6e224`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeManagedJobTemplate

## Description
The iot AWS-managed job template read path diverges from AWS on TWO independent causes: the published catalog it serves is incomplete for one template's documentParameters, and a templateName the catalog does not publish is refused with the wrong error class, status and sentence. Both are in DescribeManagedJobTemplate. No open issue covers the managed-job-template surface: the only other open iot issue (20261118T231406Z-iot-createmitigationaction-diverges-on-4-causes-rolearn-validation.md) is about CreateMitigationAction and carries none of these replay keys. A fixer must close BOTH causes before submitting.

Cause 1 of 2: the AWS-Download-File template's downloadUrl documentParameter is served without its example.
AWS: Smithy: DescribeManagedJobTemplate GET /managed-job-templates/{templateName}, output member documentParameters (list of DocumentParameter); DocumentParameter$example is 'An example illustrating a pattern that need to be replaced in a managed template job document schema' (target Example, @pattern ^[^\p{C}]+$). The catalog is AWS-published reference data — identical for every account and region, not per-run state — and for the AWS-Download-File template AWS publishes an example for the required downloadUrl parameter; the recording shows it as the URL-shaped illustration 'http://www.example.com/index.html'. Every other parameter of every managed template in the recording also carries an example, so a parameter without one is the exception, not the rule.
Emulator: answers 200 with the documentParameters list, but the entry with key 'downloadUrl' omits the example member entirely (actual: absent). The sibling parameter 'filePath' of the same template does carry its example, so the gap is this one catalog row rather than the serializer.
Why: documentParameters is the contract a client reads to learn what it must substitute when it creates a job from a managed template — AWS documents it as 'You can use the description of each key as a guidance to specify the inputs during runtime when creating a job'. A console, form generator or validation layer that renders or seeds each parameter from its example gets no illustration for the one parameter that needs a URL, and a client that asserts the published catalog is complete sees a template AWS never publishes in that form.
Fix: the published managed-job-template catalog data in the iot service's state (services/service-com-amazonaws-iot-awsiotservice/src/state/managed_job_templates.rs, the AWS-Download-File CatalogRow, whose downloadUrl CatalogParameter sets example: None around line 588) — publish the example AWS publishes for that parameter so DescribeManagedJobTemplate and ListManagedJobTemplates both serve the complete row.
Traces: iot-catalog-describe-managed-job-template#task#7.

Cause 2 of 2: a templateName the catalog does not publish is refused 404 ResourceNotFoundException instead of 400 InvalidRequestException.
AWS: Smithy: DescribeManagedJobTemplate models both InvalidRequestException (client, httpError 400, 'The request is not valid') and ResourceNotFoundException (client, httpError 404, 'The specified resource does not exist'); templateName is @required, @httpLabel, @length 1..64. For a well-formed templateName that names no published managed template, AWS treats the value as a bad request rather than a missing resource: it answers HTTP 400 with header x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/' and body message 'Invalid ManagedJobTemplate name: <templateName>'. The fixed text of that message is 'Invalid ManagedJobTemplate name: '; its single variable part is the templateName echoed verbatim from the request path. (Hypothesis: the managed catalog is AWS-owned published data rather than account state, so a name outside it can only be a caller mistake, which is why AWS classifies it as an invalid request.)
Emulator: answers HTTP 404 with x-amzn-errortype 'ResourceNotFoundException' and message 'ManagedJobTemplate with name: <templateName> not found' — wrong status, wrong modeled error shape, wrong errortype header value (it also lacks the 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/' shape AWS uses for its own 404s on this service) and wrong sentence.
Why: a client that catches the modeled InvalidRequestException to report 'you asked for a template that does not exist' never sees it — it gets a ResourceNotFoundException instead, and any retry/404-handling layer (for example one that treats 404 as 'not yet created, create it') takes a branch AWS never puts it on for a name that can never exist. Status-code assertions and error-message assertions both break.
Fix: the not-found branch of the DescribeManagedJobTemplate handler and the shared refusal sentence it uses (services/service-com-amazonaws-iot-awsiotservice/src/ops/describe_managed_job_template.rs lines 186-197, and state::managed_job_template_not_found_message in services/service-com-amazonaws-iot-awsiotservice/src/state/managed_job_templates.rs lines 621-623) — map an unpublished templateName to InvalidRequestException (400) with AWS's 'Invalid ManagedJobTemplate name: <templateName>' sentence, keeping it distinct from the existing unpublished-VERSION refusal ('Invalid ManagedJobTemplate version: <v>').
Traces: iot-describe-managed-job-template-not-found-error#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 169 fixtures, 1120 steps, 831 passed, score 74.2%, 45 divergence clusters.

Cause 1 — fixture iot-catalog-describe-managed-job-template, step task#7, operation DescribeManagedJobTemplate (status 200 both sides):
  expected $.documentParameters[key="downloadUrl"].example = 'http://www.example.com/index.html'
  actual   $.documentParameters[key="downloadUrl"].example = absent (missing)
  affected_count 1. Code: state/managed_job_templates.rs AWS-Download-File row sets `example: None` for key 'downloadUrl' while 'filePath' sets `example: Some("/path/to/file")`.

Cause 2 — fixture iot-describe-managed-job-template-not-found-error, step task#1, operation DescribeManagedJobTemplate:
  expected status 400, actual status 404
  expected $.message = 'Invalid ManagedJobTemplate name: <templateName>' (recorded with the probe's generated template name), actual = 'ManagedJobTemplate with name: <templateName> not found'
  expected $headers.x-amzn-errortype = 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', actual = 'ResourceNotFoundException'
  affected_count 1.

Model evidence (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json):
  DescribeManagedJobTemplate GET /managed-job-templates/{templateName}, code 200; input templateName (@required, @httpLabel, @length 1..64), templateVersion (@httpQuery, @pattern ^[1-9]+.[0-9]+$); errors InternalServerException(500), InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400).
  DocumentParameter members: key (@length 1..128, @pattern ^[a-zA-Z0-9_-]+$), description, regex, example (@pattern ^[^\p{C}]+$, 'An example illustrating a pattern that need to be replaced in a managed template job document schema'), optional (default false).

## Steps to Reproduce
Boot the emulator from this checkout and replay the two fixtures below, or exercise the calls directly against the iot endpoint.

Cause 1: GET /managed-job-templates/AWS-Download-File (no templateVersion) — i.e. aws iot describe-managed-job-template --template-name AWS-Download-File. Expected: 200 whose documentParameters entry with key 'downloadUrl' carries an example (AWS publishes a URL illustration, recorded as 'http://www.example.com/index.html'). Observed: 200 with that entry's example member absent. Replay: iot-catalog-describe-managed-job-template#task#7.

Cause 2: GET /managed-job-templates/<a well-formed 1..64-char name the managed catalog does not publish>. Expected: 400, header x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', body message 'Invalid ManagedJobTemplate name: <that name>'. Observed: 404, x-amzn-errortype 'ResourceNotFoundException', message 'ManagedJobTemplate with name: <that name> not found'. Replay: iot-describe-managed-job-template-not-found-error#task#1.
