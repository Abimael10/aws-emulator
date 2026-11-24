Issue-Source: trace-sweep-divergence
Issue-Trace: iot-provisioning-template-error-paths
Issue-Step: task#4
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-provisioning-template-error-paths iot-provisioning-template-lifecycle-errors iot-provisioning-template-not-found-errors
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|ListProvisioningTemplateVersions|4xx->4xx|$.message
Issue-Commit: c524fdf245ab30ebeb0886af25e07dc8ca59cca3

# iot: ListProvisioningTemplateVersions returns wrong ResourceNotFoundException message text

> _Note: this issue was identified against the code base at commit `c524fdf245ab30ebeb0886af25e07dc8ca59cca3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListProvisioningTemplateVersions

## Description
One cause.

Cause 1 of 1: ListProvisioningTemplateVersions on a provisioning-template name that does not exist answers with the right status and error code but the wrong error message text.

AWS: ListProvisioningTemplateVersions is GET /provisioning-templates/{templateName}/versions and models ResourceNotFoundException as a 404 client error (Smithy: ListProvisioningTemplateVersions.templateName is an @httpLabel, required, @length 1-36, @pattern ^[0-9A-Za-z_-]+$; errors include ResourceNotFoundException httpError 404). The message wording is recorded behavior only - the model carries no text for it. The recording shows AWS answering HTTP 404 with x-amzn-errortype ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/ and the JSON body message "Template named <templateName> not found". In that message the fixed text is "Template named " and " not found"; the only variable part is the templateName taken verbatim from the request URI label (the probe name in the recording is just one such value, not part of the template).

Emulator: same status (404) and same error type, but the message is built the other way round as "Unable to find a template named <templateName>" - different fixed text, same substituted templateName. Replay reports $.message value_mismatch (expected 'Template named sim2real-probe-provtmplmiss-82c17796 not found', got 'Unable to find a template named sim2real-probe-provtmplmiss-82c17796') on three separate fixtures, with status and error header matching.

Why: an error message is part of the contract for this error class. A client that matches, logs or asserts on the not-found text for provisioning templates - error-mapping layers that distinguish a missing template from an invalid one by message, CLI output comparisons, and test suites pinned to AWS wording - sees text AWS never emits, so the branch never fires and the failure surfaces as an unclassified error instead of a missing template.

Fix: the iot ListProvisioningTemplateVersions handler's not-found path, specifically the message template used when the provisioning-template lookup misses; the emitted text must be "Template named " + the request's templateName + " not found". Nearby open iot issues (20261118T124115Z-iot-2-net-new-causes-deleteprovisioningtemplate-deletes-a.md, 20261119T233847Z-iot-3-net-new-causes-deletepackageversion-on-a.md) touch provisioning templates but neither owns this message and neither carries this replay key.

Traces: iot-provisioning-template-error-paths#task#4 (representative), iot-provisioning-template-lifecycle-errors#task#7, iot-provisioning-template-not-found-errors#task#3.

## Evidence
Replay cluster service=iot|ListProvisioningTemplateVersions|status=404->404|keys=$.message, affected_count 3.
Expected (recorded AWS): HTTP 404, x-amzn-errortype: ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/, body {"message": "Template named <templateName> not found"}.
Actual (emulator): HTTP 404, same errortype, body {"message": "Unable to find a template named <templateName>"}.
Detail line: "$.message: value_mismatch (expected 'Template named sim2real-probe-provtmplmiss-82c17796 not found', got 'Unable to find a template named sim2real-probe-provtmplmiss-82c17796')".
Status and error code agree; only the message's fixed text diverges.

## Steps to Reproduce
1. Replay fixture iot-provisioning-template-error-paths, step task#4 (also iot-provisioning-template-lifecycle-errors#task#7 and iot-provisioning-template-not-found-errors#task#3).
2. Equivalently: call iot ListProvisioningTemplateVersions with a templateName that matches ^[0-9A-Za-z_-]{1,36}$ but was never created (GET /provisioning-templates/<name>/versions).
3. Observe HTTP 404 with errortype ResourceNotFoundException and message "Unable to find a template named <name>"; AWS returns "Template named <name> not found".
