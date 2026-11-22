Issue-Source: trace-sweep-divergence
Issue-Trace: iot-catalog-describe-managed-job-template
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-catalog-describe-managed-job-template iot-account-configuration-inspection iot-catalog-list-managed-job-templates
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.description
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.document
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="command"].description
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="command"].example
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="command"].regex
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="pathToHandler"]
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="runAsUser"].description
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="runAsUser"].example
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="runAsUser"].regex
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="pathToHandler"].description
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="pathToHandler"].example
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="pathToHandler"].regex
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="packages"].description
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="packages"].example
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="packages"].regex
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="serviceName"]
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="services"]
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[*].example
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="downloadUrl"].description
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="downloadUrl"].regex
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="filePath"].description
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="filePath"].example
Issue-Key: iot|DescribeManagedJobTemplate|2xx->2xx|$.documentParameters[key="filePath"].regex
Issue-Key: iot|ListManagedJobTemplates|2xx->2xx|$.managedJobTemplates[*]
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|$.message
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: iot|DescribeManagedJobTemplate|4xx->4xx|status
Issue-Commit: 4167e7cf5381813eabd70d9b700d048404e5c90a

# iot managed job template catalog diverges: 5 built-in templates carry emulator-authored documents, the list is mis-ordered and an unknown name is misclassified

> _Note: this issue was identified against the code base at commit `4167e7cf5381813eabd70d9b700d048404e5c90a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeManagedJobTemplate

## Description
The emulator serves its own hand-written version of the AWS-managed job-template catalog instead of the catalog AWS publishes, and it misclassifies a lookup of a template name that does not exist. The catalog is a fixed, account- and region-independent service asset: every member below is the same for every caller, so none of these values is a per-run detail. 7 causes.

Cause 1 of 7: DescribeManagedJobTemplate(AWS-Run-Command) serves an authored template body.
AWS: recorded behavior only (Smithy models DescribeManagedJobTemplate output members description/document/documentParameters but not the catalog's content). AWS answers 200 with description 'A managed job template for running a shell command.', document {"version":"1.0","steps":[{"action":{"name":"Run-Command","type":"runCommand","input":{"command":"${aws:iot:parameter:command}"},"runAsUser":"${aws:iot:parameter:runAsUser}"}}]}, and exactly two documentParameters: command (description 'A comma separated string of command, any comma contained in the command itself needs to be escaped.', regex '(.){0,4096}', example 'echo,Hello\,World!') and runAsUser (description 'Execute handler as another user. If not specified, then handler is executed as the same user as device client.', regex '(.){0,256}', example 'user1').
Emulator: answers 200 with description 'A managed job template to run a shell command on a device.', a document whose action is name 'runCommand' / type 'runHandler' with handler 'run-command.sh' and args+path inputs, a third parameter 'pathToHandler' that AWS does not publish for this template, and its own descriptions, examples and regexes ('^[^\p{C}]+$', '^[a-zA-Z0-9+._-]+$').
Why: a client that renders the catalog, or validates a job's parameter values against documentParameters[].regex before calling CreateJob, accepts values AWS rejects and rejects values AWS accepts; a job built from the returned document runs a different action with different inputs than on AWS.
Fix: the managed-job-template catalog seed data read by the DescribeManagedJobTemplate handler.
Traces: iot-catalog-describe-managed-job-template#task#1.

Cause 2 of 7: DescribeManagedJobTemplate(AWS-Reboot) serves an authored template body.
AWS: recorded behavior only. description 'A managed job template for rebooting the device.'; document action name 'Reboot' (type runHandler, handler 'reboot.sh'); runAsUser and pathToHandler carry AWS's descriptions ('Execute handler as another user. ...', 'Path to handler on the device. If not specified, then device client will use the current working directory.'), examples ('user1', '/path/to/handler/script') and regexes ('(.){0,256}', '(.){0,4096}').
Emulator: description 'A managed job template to reboot a device.', action name 'reboot', and its own parameter descriptions, examples ('root', '/tmp/device-firmware.1.0') and regexes.
Why: same as cause 1 — client-side parameter validation and the executed action both diverge.
Fix: the managed-job-template catalog seed data.
Traces: iot-account-configuration-inspection#task#3, iot-catalog-describe-managed-job-template#task#2.

Cause 3 of 7: DescribeManagedJobTemplate(AWS-Remove-Application) serves an authored template body.
AWS: recorded behavior only. description 'A managed job template for uninstalling one or more applications.'; document action name 'Remove-Application'; parameter packages with description 'A space separated list of packages to uninstall.', regex '([a-zA-Z0-9+_\-\.]+[\s]?)+' and example 'lftp dos2unix tcpdump' — i.e. a SPACE separated list.
Emulator: description 'A managed job template to remove one or more applications from a device.', action name 'removePackages', and packages described/validated as a COMMA separated list (regex '^[a-zA-Z0-9+._-]+(,[a-zA-Z0-9+._-]+)*$', example 'openssl,curl').
Why: the separator the emulator documents and validates is the opposite of AWS's, so a parameter value that works against AWS is rejected (and vice versa).
Fix: the managed-job-template catalog seed data.
Traces: iot-catalog-describe-managed-job-template#task#3, iot-catalog-describe-managed-job-template#task#5.

Cause 4 of 7: DescribeManagedJobTemplate(AWS-Stop-Application) publishes the wrong parameter name.
AWS: recorded behavior only. description 'A managed job template for stopping one or more system services.'; document action name 'Stop-Application' with handler 'stop-services.sh' and arg ${aws:iot:parameter:services}; required (optional=false) parameter key 'services', description 'A space separated list of system services to stop.', regex '([a-zA-Z0-9+_\-\.]+[\s]?)+', example 'cron httpd bluetooth'.
Emulator: action name 'stopService', handler 'stop-service.sh', arg ${aws:iot:parameter:serviceName}, and a parameter named 'serviceName' — the modeled parameter 'services' is absent entirely.
Why: a job created with the documentParameters AWS publishes ({"services": ...}) has no matching placeholder in the emulator's document, so the parameter substitution a client depends on silently does not happen.
Fix: the managed-job-template catalog seed data.
Traces: iot-catalog-describe-managed-job-template#task#4, iot-catalog-describe-managed-job-template#task#6, iot-catalog-describe-managed-job-template#task#8.

Cause 5 of 7: DescribeManagedJobTemplate(AWS-Download-File) serves an authored template body.
AWS: recorded behavior only. description 'A managed job template for downloading a file.'; document action name 'Download-File'; downloadUrl description 'URL of file to download.' regex '(.*?)'; filePath description 'Path on the device where downloaded file is written.' regex '(.*?)' example '/path/to/file'.
Emulator: description 'A managed job template to download a file to a device.', action name 'downloadFile', downloadUrl regex '^(http|https)://[^\p{C}]+$', filePath regex '^[^\p{C}]+$' example '/tmp/device-firmware.1.0', and authored parameter descriptions.
Why: the emulator's regexes are strictly narrower than AWS's permissive '(.*?)', so client-side validation refuses URLs and paths AWS accepts.
Fix: the managed-job-template catalog seed data.
Traces: iot-catalog-describe-managed-job-template#task#7.

Cause 6 of 7: ListManagedJobTemplates returns the catalog in a different order and with the authored descriptions.
AWS: recorded behavior only. Smithy: ListManagedJobTemplates is @paginated over managedJobTemplates. AWS's first four summaries are AWS-Run-Command, AWS-Reboot, AWS-Remove-Application, AWS-Stop-Application, each carrying the AWS description text quoted in causes 1-5, environments ['LINUX'] and templateVersion '1.0'.
Emulator: returns the catalog in its own (alphabetical) order — AWS-Download-File, AWS-Install-Application, AWS-Reboot, AWS-Remove-Application, ... — so every positional element diverges, and each summary carries the authored description instead of AWS's.
Why: a client that reads managedJobTemplates[0] (or pages with maxResults=1) gets a different template than on AWS, and the description it displays is not the service's.
Fix: the managed-job-template catalog seed data and the ordering applied by the ListManagedJobTemplates handler.
Traces: iot-catalog-list-managed-job-templates#task#1.

Cause 7 of 7: DescribeManagedJobTemplate for a name that is not in the catalog is refused as invalid input instead of not-found.
AWS: Smithy: DescribeManagedJobTemplate errors include ResourceNotFoundException (client, httpError 404) and InvalidRequestException (client, httpError 400); templateName is an @httpLabel with @length 1..64. A syntactically valid name that names no managed template is a not-found: AWS answers 404 with x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/' and message 'ManagedJobTemplate with name: <templateName> not found' — fixed text 'ManagedJobTemplate with name: ' and ' not found' around the templateName taken from the request path.
Emulator: answers 400 with x-amzn-errortype 'InvalidRequestException' (also missing AWS's ':http://iot.amazonaws.com/doc/2015-10-02/' suffix) and message 'Invalid ManagedJobTemplate name: <templateName>'.
Why: a client that distinguishes a missing template (retry/create) from a malformed request (abort) takes the wrong branch, and SDK error-type matching on ResourceNotFoundException never fires.
Fix: the DescribeManagedJobTemplate handler's lookup-miss path and its error serializer.
Traces: iot-account-configuration-inspection#task#4.

## Evidence
Cause 1 (iot-catalog-describe-managed-job-template#task#1, 200 vs 200): $.description expected 'A managed job template for running a shell command.', got 'A managed job template to run a shell command on a device.'; $.document expected action {"name":"Run-Command","type":"runCommand","input":{"command":"${aws:iot:parameter:command}"}}, got {"name":"runCommand","type":"runHandler","input":{"handler":"run-command.sh","args":[...],"path":"${aws:iot:parameter:pathToHandler}"}}; $.documentParameters[key="command"].regex expected '(.){0,4096}', got '^[^\\p{C}]+$'; $.documentParameters[key="pathToHandler"] extra (expected None).
Cause 2 (iot-account-configuration-inspection#task#3): $.description expected 'A managed job template for rebooting the device.', got 'A managed job template to reboot a device.'; document action name expected 'Reboot', got 'reboot'; runAsUser.regex expected '(.){0,256}', got '^[a-zA-Z0-9+._-]+$'; pathToHandler.regex expected '(.){0,4096}', got '^[^\\p{C}]+$'.
Cause 3 (iot-catalog-describe-managed-job-template#task#3): $.documentParameters[key="packages"].regex expected '([a-zA-Z0-9+_\\-\\.]+[\\s]?)+', got '^[a-zA-Z0-9+._-]+(,[a-zA-Z0-9+._-]+)*$'; description expected 'A space separated list of packages to uninstall.', got 'The packages to remove, separated by commas.'; action name expected 'Remove-Application', got 'removePackages'.
Cause 4 (iot-catalog-describe-managed-job-template#task#4): $.documentParameters[key="services"] missing (expected key 'services', optional False, regex '([a-zA-Z0-9+_\\-\\.]+[\\s]?)+'); emulator serves 'serviceName' instead; document action expected name 'Stop-Application' handler 'stop-services.sh', got name 'stopService' handler 'stop-service.sh'.
Cause 5 (iot-catalog-describe-managed-job-template#task#7): $.documentParameters[key="downloadUrl"].regex expected '(.*?)', got '^(http|https)://[^\\p{C}]+$'; filePath.regex expected '(.*?)', got '^[^\\p{C}]+$'; action name expected 'Download-File', got 'downloadFile'.
Cause 6 (iot-catalog-list-managed-job-templates#task#1): $.managedJobTemplates[0] expected templateName 'AWS-Run-Command', got 'AWS-Download-File'; [1] expected 'AWS-Reboot', got 'AWS-Install-Application'; [2] expected 'AWS-Remove-Application', got 'AWS-Reboot'; [3] expected 'AWS-Stop-Application', got 'AWS-Remove-Application' (all eight positions diverge).
Cause 7 (iot-account-configuration-inspection#task#4): status expected 404, got 400; $.message expected 'ManagedJobTemplate with name: AWS-Does-Not-Exist not found', got 'Invalid ManagedJobTemplate name: AWS-Does-Not-Exist'; $headers.x-amzn-errortype expected 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', got 'InvalidRequestException'.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: GET /managed-job-templates/AWS-Run-Command (fixture iot-catalog-describe-managed-job-template, step task#1).
3. Cause 2: GET /managed-job-templates/AWS-Reboot (fixture iot-account-configuration-inspection, step task#3; also iot-catalog-describe-managed-job-template#task#2).
4. Cause 3: GET /managed-job-templates/AWS-Remove-Application (iot-catalog-describe-managed-job-template#task#3 and #task#5).
5. Cause 4: GET /managed-job-templates/AWS-Stop-Application (iot-catalog-describe-managed-job-template#task#4, #task#6, #task#8).
6. Cause 5: GET /managed-job-templates/AWS-Download-File (iot-catalog-describe-managed-job-template#task#7).
7. Cause 6: GET /managed-job-templates (iot-catalog-list-managed-job-templates#task#1) and compare managedJobTemplates[] element by element.
8. Cause 7: GET /managed-job-templates/AWS-Does-Not-Exist (iot-account-configuration-inspection#task#4) and observe 400 InvalidRequestException instead of 404 ResourceNotFoundException.
