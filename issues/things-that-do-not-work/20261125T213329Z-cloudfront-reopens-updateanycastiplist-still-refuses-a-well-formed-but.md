Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected|run#7|AssertionError|assert c_malformed_body == "EntityNotFound", c_malformed_body
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# REOPENS: UpdateAnycastIpList still refuses a well-formed-but-absent Anycast-IP-list Id written in the entity-id alphabet with 400 InvalidArgument "Id format is invalid" instead of 404 EntityNotFound

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateAnycastIpList

## Description
REOPENS 20261114T135924Z-cloudfront-2-net-new-causes-a-response-headers.md: its fix left the call refused.

That issue's Cause 2 claimed UpdateAnycastIpList had been made to resolve its target before reading anything the body submits, and its Resolution note states that "the operations addressed by an Anycast static IP list `Id` read of the value only whether it is written in a form CloudFront writes an identifier in at all - under a namespace label, or in the alphabet an entity identifier is written in - and resolve every such value against the store". The second half of that claim did not land: only the namespace-label form is carried through to the store, so the call is still REFUSED, with the same 400/InvalidArgument the earlier issue reported, for an Id written in the entity-id alphabet. This is one cause (Cause 1 of 1).

Cause 1 of 1: the pre-lookup identifier-format gate of UpdateAnycastIpList rejects an Anycast-IP-list Id that carries no namespace separator, so the absent-list condition can never be reported for such an Id.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json): UpdateAnycastIpList is PUT /2020-05-31/anycast-ip-list/{Id}; Id is a plain `string` @httpLabel, required, with NO @pattern and NO @length, and IfMatch is a required `string` in the If-Match header. Its modelled errors are AccessDenied (403), EntityNotFound (404, doc "The entity was not found."), InvalidArgument (400, doc "An argument is invalid."), InvalidIfMatchVersion (400), PreconditionFailed (412), UnsupportedOperation (400). Because the member carries no constraint, neither the namespace label, nor the width, nor the alphabet of the identifier is something the service may refuse a value on; an identifier that is simply unknown is the ABSENT list. Recorded behavior: UpdateAnycastIpList resolves its target FIRST, so a well-formed-but-never-issued Anycast-IP-list Id is refused with error code EntityNotFound / HTTP 404 both for a malformed body (IpamCidrConfigs with an unparseable Cidr) and for a well-formed body (IpAddressType=ipv4). The requirement is the status and the error code; the message is AWS's fixed entity-not-found sentence for this resource kind ("The specified Anycast IP list does not exist" - the whole sentence is fixed text and takes no value from the request), which the emulator already owns as crate::state::anycast_ip_list_not_found() and simply never reaches for these inputs.

Emulator: the call is refused 400 InvalidArgument with the emulator-authored message "Id format is invalid" (crate::state::invalid_id_format("Id")). The gate runs before the store is consulted and admits ONLY the namespaced form: crate::state::is_namespaced_resource_id requires a '_' or '-' separator, a non-empty lowercase label of at most 3 letters before it, and an alphanumeric/separator body. An Id written in the alphabet CloudFront writes an entity identifier in - decimal digits and uppercase ASCII, no separator, which the same module already recognises as crate::state::is_entity_id_form - finds no separator and is refused. So for that whole class of identifiers the modelled EntityNotFound branch of this operation is unreachable, and the recorded ordering (target resolved before body validation) cannot be observed: the InvalidArgument the body's unparseable CIDR would also produce is indistinguishable from the one the id gate produces. GetAnycastIpList (src/ops/get_anycast_ip_list.rs lines 164-169) carries the identical gate, so the same class of id is refused on the read path too.

Why: a client cannot distinguish "this Anycast static IP list does not exist" from "you sent me an unusable argument". Reconciliation and retry logic keyed on EntityNotFound/404 (the resource drifted away - recreate it, stop polling) never runs; the 400 InvalidArgument path is taken instead, which callers treat as a permanently bad request and surface as a programming error, so an ordinary id drift is reported as a client bug and never healed. A generated SDK's typed-error handling for the modelled EntityNotFound of this operation is dead code for ids of this form.

Fix: the pre-lookup identifier gate in UpdateAnycastIpList's validation (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/update_anycast_ip_list.rs, the `is_namespaced_resource_id` check before the store lock) and the shared predicate pair in src/state/identity.rs (`is_namespaced_resource_id` / `is_entity_id_form`): an Id written in a form CloudFront writes an identifier in at all - under a namespace label OR in the entity-id alphabet - must reach the store lookup and be answered 404 EntityNotFound with this resource's fixed not-found sentence, before the If-Match precondition and before IpamCidrConfigs/IpAddressType are read. The same gate on the read path (src/ops/get_anycast_ip_list.rs) belongs in the same change, since the two routes must not disagree about what an identifier is.

Traces: cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected.

## Evidence
Failing check cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected (verdict: mismatch):
  File "check_cloudfront_update_anycast_ip_list_malformed_ip_invalid_argument_rejected.py", line 36, in run
    assert c_malformed_body == "EntityNotFound", c_malformed_body
  AssertionError: InvalidArgument

Request: ListAnycastIpLists (positive control - succeeds, so the surface is reachable and this is not a blanket refusal), then UpdateAnycastIpList with Id='IAL1A2B3C4D5E6' (never issued; digits+uppercase, no separator), IfMatch='ETVPDKIKX0DER', IpamCidrConfigs=[{Cidr: '300.1.1.0/24' (an unparseable CIDR), IpamPoolArn: arn:aws:ec2:...:ipam-pool/...}].
  Expected (AWS): error code EntityNotFound, HTTP 404 (and the same code for the follow-up call with the well-formed body IpAddressType='ipv4').
  Actual (emulator): error code InvalidArgument, HTTP 400, Message 'Id format is invalid'.

Emulator code path: services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/update_anycast_ip_list.rs lines 184-189 return InvalidArgument via crate::state::invalid_id_format("Id") when !crate::state::is_namespaced_resource_id(&input.id), before `state.lock()`; src/state/identity.rs lines 1133-1146 show is_namespaced_resource_id returns false for any value with no '_' or '-' separator, while lines 1161-1163 define is_entity_id_form (digits + uppercase ASCII, no separator) which this handler never consults; the correct answer crate::state::anycast_ip_list_not_found() is already constructed at lines 204-207 and 240-243 of the handler but is unreachable for this id class. src/ops/get_anycast_ip_list.rs lines 164-169 carry the identical gate.

The Id and IfMatch literals are the check's probe values filling the template; the requirement is that an identifier of this mint's alphabet is resolved against the store and answered 404 EntityNotFound, before the body is read.

## Steps to Reproduce
1. Build and boot the emulator and replay the behavioral suite for service cloudfront.
2. Call ListAnycastIpLists (positive control; it succeeds and returns AnycastIpLists.Items).
3. Call UpdateAnycastIpList with a never-issued Id written in the entity-id alphabet (digits + uppercase ASCII, no separator, e.g. 'IAL1A2B3C4D5E6'), any IfMatch token, and IpamCidrConfigs carrying an unparseable CIDR (e.g. Cidr='300.1.1.0/24'). Observe 400 InvalidArgument with Message 'Id format is invalid'; AWS answers 404 EntityNotFound.
4. Repeat with a well-formed body (IpAddressType='ipv4') and the same Id: still 400 InvalidArgument, because the refusal comes from the pre-lookup id gate, not from body validation.
5. For contrast, call UpdateAnycastIpList with a namespaced-but-absent Id (e.g. 'aip_1A2B3C4D5E6F7G8H9J0K1L'): that one reaches the store and is answered EntityNotFound, which isolates the defect to the id-form gate.
6. Re-replay cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected to confirm a fix.
