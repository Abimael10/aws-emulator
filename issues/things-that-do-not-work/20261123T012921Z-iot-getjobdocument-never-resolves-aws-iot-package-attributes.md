Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.e2e-package-version-publish-to-job-deployment
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.e2e-package-version-publish-to-job-deployment|run#24|AssertionError|assert firmware_url in doc, (
Issue-Commit: 8478c6c242353a42daf0a6c7e0b79073df4de430

# iot: GetJobDocument never resolves ${aws:iot:package:...:attributes:...} placeholders from a job's destinationPackageVersions, so the device receives the literal placeholder

> _Note: this issue was identified against the code base at commit `8478c6c242353a42daf0a6c7e0b79073df4de430`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJobDocument

## Description
Cause 1 of 1: a job created with `destinationPackageVersions` naming a PUBLISHED package version must have the Software Package Catalog placeholders in its job document resolved to that version's attributes when the document is served post-substitution. The emulator resolves only the job's own `documentParameters` and leaves every `${aws:iot:package:...}` placeholder in the document verbatim. Failing check: iot.e2e-package-version-publish-to-job-deployment.

AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json) documents GetJobDocumentRequest$beforeSubstitution as "Provides a view of the job document before and after the substitution parameters have been resolved with their exact values" (BeforeSubstitutionFlag, default false), and CreateJobRequest$destinationPackageVersions as "The package version Amazon Resource Names (ARNs) that are installed on the device when the job successfully completes. The package version must be in either the Published or Deprecated state when the job deploys. ... Up to 25 package version ARNs are allowed." AWS's Software Package Catalog defines the job-document placeholder syntax `${aws:iot:package:<packageName>:version:<versionName>:attributes:<attributeKey>}`; when a job references that package version through destinationPackageVersions, the SERVICE substitutes the placeholder with the value of that attribute on the referenced package version. So GetJobDocument with beforeSubstitution=false (the default) must return a document in which no `${aws:iot:package` substring survives and each such placeholder has been replaced by the attribute value recorded on the named package version; GetJobDocument with beforeSubstitution=true must return the document as written, placeholders intact. The package name, version name, attribute key and attribute value in that document are all values taken from the request and the emulator's state — never from any one recording.

Emulator: CreateJob accepts and now persists destinationPackageVersions (DescribeJob reports it back correctly in this check), and UpdatePackageVersion moves the version to PUBLISHED, but GetJobDocument's post-substitution view performs only `documentParameters` resolution. The reply is HTTP 200 whose `document` is byte-for-byte the document as created: `{"firmwareUrl": "${aws:iot:package:<packageName>:version:<versionName>:attributes:firmwareUrl}"}`. The attribute value stored on the published package version (`attributes.firmwareUrl`) is never consulted, and the unresolved `${aws:iot:package` placeholder leaks into the served document. No error is raised, so the divergence is silent.

Why: an OTA/deployment client puts the download URL (or any per-version attribute) into the job document only as a package placeholder, precisely so that the service fills it in from the catalog. Against the emulator the device is handed the literal placeholder string instead of the URL, so the device cannot fetch the firmware and the deployment fails — or worse, the device treats the placeholder text as a real URL. A client has no way to notice: the call returns 200 with a well-formed JSON document, and beforeSubstitution=false is the default, so the caller believes it is reading the resolved view.

Fix: the job-document substitution path shared by GetJobDocument and the job-execution document delivery — `crate::state::resolve_job_document` must additionally resolve `${aws:iot:package:<pkg>:version:<ver>:attributes:<key>}` against the package versions named by the job's persisted destinationPackageVersions (looking the attribute up in the package-version state), and leave a placeholder naming a package version the job does not reference as written. GetJobDocument's handler must keep returning the as-written document when beforeSubstitution=true. Note: the open issue 20261114T182119Z-iot-package-version-auto-versioning-never-happens-a.md does NOT cover this — that one is about the reserved "$package" named shadow never being written on a SUCCEEDED execution, and the open issue 20261106T201842Z-iot-jobs-plane-8-net-new-causes-createjob.md covers the jobs plane's create/execution-listing causes; neither touches package-attribute substitution in the document.

Traces: failing check iot.e2e-package-version-publish-to-job-deployment (assertion `firmware_url in doc` on the GetJobDocument reply). Supporting file citations in the checkout: services/service-com-amazonaws-iot-awsiotservice/src/ops/get_job_document.rs lines 183-207 (the post-substitution view calls crate::state::resolve_job_document with only the job's document_parameters) and its module doc lines 10-15 ("A placeholder the job has no parameter for is reported as written").

## Evidence
Expected (AWS): after create_package(P) / create_package_version(P, "1.0.0", attributes={"firmwareUrl": U}) / update_package_version(..., action="PUBLISH") and create_job(jobId=J, targets=[thingArn], targetSelection="SNAPSHOT", document='{"firmwareUrl": "${aws:iot:package:P:version:1.0.0:attributes:firmwareUrl}"}', destinationPackageVersions=[<packageVersionArn>]), GetJobDocument(jobId=J) (beforeSubstitution defaulting to false) returns HTTP 200 with document = '{"firmwareUrl": "<U>"}' — the attribute value recorded on the published package version — and no `${aws:iot:package` substring anywhere in it. GetJobDocument(jobId=J, beforeSubstitution=true) returns the document as written, with the placeholder intact.

Actual (emulator): GetJobDocument(jobId=J) returns HTTP 200 with document = '{"firmwareUrl": "${aws:iot:package:<packageName>:version:1.0.0:attributes:firmwareUrl}"}' — the placeholder as written. The check fails on `assert firmware_url in doc` with "job document was not substituted with the published package version's attribute: {\"firmwareUrl\": \"${aws:iot:package:<packageName>:version:1.0.0:attributes:firmwareUrl}\"}" (the package name shown in that message is a per-run generated name; the fixed part of the divergence is that the `${aws:iot:package:...:attributes:...}` form survives substitution). The preceding steps all agree with AWS: get_package_version reports status PUBLISHED, and describe_job reports destinationPackageVersions == [<packageVersionArn>].

## Steps to Reproduce
1. iot.get_indexing_configuration(); if needed iot.update_indexing_configuration(thingIndexingConfiguration={thingIndexingMode:"REGISTRY", namedShadowIndexingMode:"ON", filter:{namedShadowNames:["$package"]}}) — the Software Package Catalog prerequisite for CreatePackage.
2. iot.create_thing(thingName=T) → note thingArn.
3. iot.create_package(packageName=P).
4. iot.create_package_version(packageName=P, versionName="1.0.0", attributes={"firmwareUrl": "https://example-bucket.s3.us-west-2.amazonaws.com/firmware-image.bin"}).
5. iot.update_package_version(packageName=P, versionName="1.0.0", action="PUBLISH"); iot.get_package_version(packageName=P, versionName="1.0.0") → status "PUBLISHED", note packageVersionArn.
6. iot.create_job(jobId=J, targets=[thingArn], targetSelection="SNAPSHOT", document='{"firmwareUrl": "${aws:iot:package:P:version:1.0.0:attributes:firmwareUrl}"}', destinationPackageVersions=[packageVersionArn]).
7. iot.describe_job(jobId=J) → destinationPackageVersions == [packageVersionArn] (this already agrees).
8. iot.get_job_document(jobId=J). AWS: document contains the firmware URL and no "${aws:iot:package" substring. Emulator: document is returned with the placeholder unresolved.
9. iot.get_job_document(jobId=J, beforeSubstitution=True) must still return the as-written document with the placeholder intact.
