Issue-Source: trace-sweep-divergence
Issue-Trace: iot-principal-policy-thing-attachment-lifecycle
Issue-Step: task#18
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-principal-policy-thing-attachment-lifecycle iot-thing-principal-lifecycle iot-thing-group-untag-noop-and-validation-errors iot-vpce-traj-attach-target-mistakes iot-domain-configuration-invalid-certificate-rejected
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|AttachThingPrincipal|4xx->4xx|$.message
Issue-Key: iot|ListThingPrincipals|4xx->4xx|$.message
Issue-Key: iot|UntagResource|4xx->4xx|$.message
Issue-Key: iot|AttachThingPrincipal|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: iot|AttachThingPrincipal|4xx->4xx|status
Issue-Key: iot|CreateDomainConfiguration|4xx->4xx|$.message
Issue-Commit: 53152f24252eb4c6b050f7ba33da4a3e7b513fd2

# iot refusals diverge on 6 causes: not-found and invalid-principal messages are emulator-authored and two are mis-classed

> _Note: this issue was identified against the code base at commit `53152f24252eb4c6b050f7ba33da4a3e7b513fd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachThingPrincipal

## Description
Six net-new causes, all in the same fault: the iot emulator's refusal paths emit messages it authored itself instead of the text AWS words for that error class, and two of the six additionally pick the wrong error class/status. All six are one service (iot) and each is numbered below with its own fixture and step.

These are separate causes from the already-open 'iot refusal text is emulator-authored on six operations' (20261115T161531Z-iot-refusal-text-is-emulator-authored-on-six.md), 'iot refusal-message fidelity: 7 causes' (20261114T082236Z-iot-refusal-message-fidelity-7-causes-addthingtothinggroup-createstream.md) and 'iot UpdateThing's ResourceNotFoundException message is account-qualified' (20261115T093926Z-iot-updatething-s-resourcenotfoundexception-message-is-account-qualified.md): none of those issues carries the replay keys below, and causes 1-3 show the same account-qualified '<Type> <account>:<name> not found.' template that the UpdateThing issue reports, so a single shared not-found formatter is the likely origin. Domain-configuration causes already open ('iot domain-configuration plane: 7 causes', 'iot: 2 net-new causes - DeleteProvisioningTemplate ... and CreateDomainConfigur...') likewise do not cover cause 6.

Cause 1 of 6: AttachThingPrincipal against a thing that does not exist.
AWS: Smithy: AttachThingPrincipal errors include ResourceNotFoundException (httpError 404, member message). The recorded response is 404 with message "Failed to attach the principal <principal> to thing <thingName> because the thing does not exist in your account". Fixed text: "Failed to attach the principal ", " to thing ", " because the thing does not exist in your account" (no trailing period). Variable parts: <principal> is the principal from the x-amzn-principal header of the request and <thingName> is the {thingName} URI label; the recorded certificate ARN and thing name are one run's values filling that template, not part of it.
Emulator: answers 404 but with message "Thing <account-id>:<thingName> not found." - a different sentence, account-qualified, with a trailing period, and dropping the principal entirely.
Why: a client that surfaces or matches on the service's refusal text gets a message that names neither the operation that failed nor the principal involved, so operator-facing output and any text-matching retry/branching logic disagree with AWS.
Fix: the AttachThingPrincipal handler's not-found refusal path (the message it builds for ResourceNotFoundException); if a shared not-found formatter mints "<Type> <account>:<name> not found." for the whole registry, the per-operation wording has to override it.
Traces: iot-principal-policy-thing-attachment-lifecycle#task#18, iot-thing-principal-lifecycle#task#7.

Cause 2 of 6: ListThingPrincipals against a thing that does not exist.
AWS: Smithy: ListThingPrincipals errors include ResourceNotFoundException (httpError 404). The recorded response is 404 with message "Failed to list principals for thing<thingName> because the thing does not exist in your account". Fixed text: "Failed to list principals for thing" and " because the thing does not exist in your account" - note AWS emits no space between "thing" and the name, and that quirk is the requirement. Variable part: <thingName> from the {thingName} URI label.
Emulator: answers 404 with "Thing <account-id>:<thingName> not found." instead.
Why: same as cause 1 - the refusal text a client logs or matches differs from AWS for an ordinary missing-resource read.
Fix: the ListThingPrincipals handler's not-found refusal path.
Traces: iot-principal-policy-thing-attachment-lifecycle#task#20.

Cause 3 of 6: UntagResource on a thing group that does not exist.
AWS: Smithy: UntagResource errors include ResourceNotFoundException (httpError 404). The recorded response is 404 with message "Thing Group <name> does not exist." Fixed text: "Thing Group " and " does not exist." (two words, trailing period). Variable part: <name>, the thing-group name resolved from the request's resourceArn.
Emulator: answers 404 with "ThingGroup <account-id>:<name> not found." - the resource type is spelled as one word, the name is account-qualified and the sentence is "not found." rather than "does not exist.".
Why: a client cannot distinguish AWS's tag-plane not-found text from the emulator's, and the account-qualified name leaks an identifier AWS never puts in this message.
Fix: the UntagResource handler's resource-resolution refusal path (shared not-found formatter as in cause 1).
Traces: iot-thing-group-untag-noop-and-validation-errors#task#6.

Cause 4 of 6: AttachThingPrincipal given a bare certificate id rather than a principal ARN.
AWS: Smithy: AttachThingPrincipal.principal targets Principal (plain string, bound to header x-amzn-principal) and the operation models InvalidRequestException (httpError 400). The recorded response is 400 with message "Invalid principal type: <principal>". Fixed text: "Invalid principal type: ". Variable part: <principal>, echoed verbatim from the request header (the recorded 64-hex certificate id is one run's value).
Emulator: answers 400 but with "Invalid principal: <principal>. A thing principal is a certificate ARN or an Amazon Cognito identity of the form region:id" - it drops the word "type" and appends an explanatory sentence AWS does not send.
Why: the status and class are right but the text is not AWS's, so a client that asserts on or displays the refusal reason sees emulator-authored guidance.
Fix: the AttachThingPrincipal handler's principal validation (the message it attaches to InvalidRequestException).
Traces: iot-vpce-traj-attach-target-mistakes#task#8.

Cause 5 of 6: AttachThingPrincipal given a well-formed IoT policy ARN as the principal - wrong error class, not just wrong text.
AWS: Smithy: AttachThingPrincipal models both InvalidRequestException (400) and ResourceNotFoundException (404). A syntactically valid principal ARN that names no principal in the account is a NOT-FOUND condition: the recorded response is 404 with message "Can't attach the principal <principal> to thing <thingName> because the principal does not exist in your account". Fixed text: "Can't attach the principal ", " to thing ", " because the principal does not exist in your account". Variable parts: <principal> from the x-amzn-principal header, <thingName> from the URI label. The error header must therefore name ResourceNotFoundException.
Emulator: answers 400 with x-amzn-errortype "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/" and message "Invalid principal: <principal>. A thing principal is a certificate ARN or an Amazon Cognito identity of the form region:id" - it treats an unresolvable-but-valid ARN as a syntax error.
Why: this is the cause a client branches on. An SDK maps 404/ResourceNotFoundException to a retryable/creatable condition and 400/InvalidRequestException to a permanent caller bug, so attaching a principal that simply has not been created yet is reported as an unfixable malformed request; it also means the emulator's principal validator accepts only certificate ARNs and Cognito identities as *shapes* and never consults state.
Fix: the AttachThingPrincipal handler's principal validation and lookup order - validate the principal's syntax first, then resolve it against state and raise ResourceNotFoundException (404) with the wording above when it resolves to nothing.
Traces: iot-vpce-traj-attach-target-mistakes#task#9.

Cause 6 of 6: CreateDomainConfiguration's certificate-validation refusal names a certificate ARN that is not the one the request supplied.
AWS: Smithy: CreateDomainConfiguration models CertificateValidationException (httpError 400) and takes serverCertificateArns (list, max 1) and validationCertificateArn (AcmCertificateArn, @pattern ^arn:aws(-cn|-us-gov|-iso-b|-iso)?:acm:...:certificate/[a-zA-Z0-9/-]+$). The recorded response is 400 with message "Certificate <certificateArn> does not exist." Fixed text: "Certificate " and " does not exist." Variable part: <certificateArn> - the ACM certificate ARN taken from the request member that failed validation. The recorded ARN is one run's value; the requirement is that the emulator echo the request's ARN.
Emulator: answers 400 with the right fixed text but substitutes a different certificate ARN than the request's failing one, so the message identifies the wrong certificate. Hypothesis: the handler reports an ARN drawn from its own state or from the other certificate member of the request rather than the member it rejected.
Why: the whole value of this message is telling the caller WHICH certificate is missing. A client that parses the ARN out of the refusal (or an operator who copies it) chases a certificate that was never in the request, and an idempotent create/retry loop keyed on that ARN never converges.
Fix: the CreateDomainConfiguration handler's certificate-validation path - format the message from the request member being validated (validationCertificateArn / serverCertificateArns[0]) rather than from any other source.
Traces: iot-domain-configuration-invalid-certificate-rejected#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 (169 fixtures, 1120 steps). All six clusters are verdict=mismatch, service=iot, stub_path empty, owned_by empty.

Cause 1 - AttachThingPrincipal, expected 404 / actual 404, cluster covers 2 steps:
  $.message: expected "Failed to attach the principal arn:aws:iot:us-west-2:<acct>:cert/<certId> to thing <thing> because the thing does not exist in your account"
                 actual "Thing <acct>:<thing> not found."

Cause 2 - ListThingPrincipals, expected 404 / actual 404:
  $.message: expected "Failed to list principals for thing<thing> because the thing does not exist in your account"
                 actual "Thing <acct>:<thing> not found."

Cause 3 - UntagResource, expected 404 / actual 404:
  $.message: expected "Thing Group <name> does not exist."
                 actual "ThingGroup <acct>:<name> not found."

Cause 4 - AttachThingPrincipal, expected 400 / actual 400:
  $.message: expected "Invalid principal type: <principal>"
                 actual "Invalid principal: <principal>. A thing principal is a certificate ARN or an Amazon Cognito identity of the form region:id"

Cause 5 - AttachThingPrincipal, expected 404 / actual 400:
  status: expected 404, got 400
  $.message: expected "Can't attach the principal <policyArn> to thing <thing> because the principal does not exist in your account"
                 actual "Invalid principal: <policyArn>. A thing principal is a certificate ARN or an Amazon Cognito identity of the form region:id"
  $headers.x-amzn-errortype: got "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/" where a 404 requires ResourceNotFoundException

Cause 6 - CreateDomainConfiguration, expected 400 / actual 400:
  $.message: expected "Certificate arn:aws:acm:us-west-2:<acct>:certificate/<arnA> does not exist."
                 actual "Certificate arn:aws:acm:us-west-2:<acct>:certificate/<arnB> does not exist."  (arnB is not the ARN the replayed request carried)

Model evidence (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json): AttachThingPrincipal -> PUT /things/{thingName}/principals, principal bound to header x-amzn-principal, errors {InvalidRequestException 400, ResourceNotFoundException 404}; ListThingPrincipals -> GET /things/{thingName}/principals, errors include ResourceNotFoundException 404; UntagResource -> POST /untag, errors include ResourceNotFoundException 404; CreateDomainConfiguration errors include CertificateValidationException 400 and InvalidRequestException 400; ResourceNotFoundException/InvalidRequestException each carry a single `message` member.

## Steps to Reproduce
Boot the emulator from this checkout and replay the corpus (replay_wire_corpus), or drive the steps directly:

Cause 1: create a thing and a certificate; PUT /things/<missing-thing>/principals with x-amzn-principal: <certificate ARN> for a thing name that does not exist. Expect 404 "Failed to attach the principal <arn> to thing <name> because the thing does not exist in your account". Replay iot-principal-policy-thing-attachment-lifecycle#task#18 (also iot-thing-principal-lifecycle#task#7).

Cause 2: GET /things/<missing-thing>/principals. Expect 404 "Failed to list principals for thing<name> because the thing does not exist in your account". Replay iot-principal-policy-thing-attachment-lifecycle#task#20.

Cause 3: POST /untag with resourceArn of a thing-group ARN that does not exist and a non-empty tagKeys list. Expect 404 "Thing Group <name> does not exist.". Replay iot-thing-group-untag-noop-and-validation-errors#task#6.

Cause 4: PUT /things/<existing-thing>/principals with x-amzn-principal set to a bare 64-hex certificate id (not an ARN). Expect 400 "Invalid principal type: <value>". Replay iot-vpce-traj-attach-target-mistakes#task#8.

Cause 5: PUT /things/<existing-thing>/principals with x-amzn-principal set to a well-formed IoT policy ARN. Expect 404 / ResourceNotFoundException "Can't attach the principal <arn> to thing <name> because the principal does not exist in your account". Replay iot-vpce-traj-attach-target-mistakes#task#9.

Cause 6: POST /domainConfigurations/<name> with a validationCertificateArn that matches the AcmCertificateArn pattern but names no certificate. Expect 400 "Certificate <that same ARN> does not exist." and compare the ARN in the message to the one sent. Replay iot-domain-configuration-invalid-certificate-rejected#task#1.
