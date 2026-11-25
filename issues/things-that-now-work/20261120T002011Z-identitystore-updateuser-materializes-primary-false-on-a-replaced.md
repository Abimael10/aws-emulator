Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: identitystore.update-user-phone-numbers-full-replace
Issue-Shape-Id: com.amazonaws.identitystore#AWSIdentityStore
Issue-Key: identitystore.update-user-phone-numbers-full-replace|run#15|AssertionError|assert phones[0].get("Primary") is not False, (
Issue-Commit: 602156962253e64136e9b0eeef8fba5e51605e54

# identitystore UpdateUser materializes Primary=false on a replaced multi-valued element AWS reports as absent

> _Note: this issue was identified against the code base at commit `602156962253e64136e9b0eeef8fba5e51605e54`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateUser

## Description
One root cause in the identitystore emulator, found by replaying the behavioral suite (simparity-behavioral-aws-v1).

NOTE ON PRIOR ART: this is the same defect previously filed as Cause 4 of 20261029T220001Z-identitystore-non-idempotent-deletes-creates-accepted-into-a.md, which is now marked FIXED. The fix did not land for this path — the decode seam it named is unchanged and the check still fails identically, so the cause is re-filed here. The call itself is NOT refused (UpdateUser and DescribeUser both return HTTP 200); the divergence is purely in the represented value.

Cause 1 of 1: UpdateUser replacing a multi-valued attribute element stores and serializes an unspecified `Primary` as the boolean `false`, where AWS leaves the member ABSENT.

AWS: Smithy `com.amazonaws.identitystore#PhoneNumber` has members Value, Type and Primary; Primary targets BooleanType with `"default": false`. `User$PhoneNumbers` (and Emails, Addresses, Roles) are lists with `@length min 1 max 1`. `UpdateUser.Operations` is a required `AttributeOperations` list (length 1..100) of AttributeOperation, documented as "An operation that applies to the requested group. This operation might add, replace, or remove an attribute" — an AttributePath naming a multi-valued attribute (`phoneNumbers`) replaces the WHOLE list element; AttributePath cannot name a single sub-attribute of a list element, so there is nothing for the service to merge into. Recorded behavior: an element that explicitly carries `Primary: false` at CreateUser is echoed back by DescribeUser with `Primary: false`, but after an UpdateUser whose replacement element supplies only `Value`, the following DescribeUser reports that element with NO Primary member at all — unspecified sub-attributes of a replaced element are absent in the response, not materialized from the model's `@default`. `Type` behaves the same way, and the emulator already drops `Type` correctly.

Emulator: after the replace, DescribeUser reports the element as `{'Value': <new number>, 'Primary': False}` — Type correctly absent, Primary present and false. Because the pre-update element also carried `Primary: false`, the result is indistinguishable from a field-level MERGE of the old element into the new one, which is how the check reads it. The decode seam coerces the missing member: `fn flag` in ops/update_user.rs maps `None | Some(Document::Null) => Ok(false)` ("defaulting to the model's own @default: false"), and `fn phone` / `fn email` / `fn role` / `fn address` / `fn photo` all build their entry with `primary: flag(members.get("Primary").copied(), path)?`, so the stored entry always holds a concrete boolean the serializer then emits.

Why: a client cannot distinguish "Primary was never set" from "Primary was explicitly set to false". A SCIM-style sync or any reconciler that diffs the described user against its desired state sees a `Primary: false` it never sent, and therefore either reports perpetual drift or concludes the replace did not take effect. It also contradicts UpdateUser's whole-element replacement semantics for list-valued attributes: the surviving sub-attribute makes the operation look like a merge.

Fix: the multi-valued attribute decode/representation seam of the UpdateUser handler and the user state it writes — the `flag` helper and the `phone`/`email`/`role`/`address`/`photo` entry decoders in the UpdateUser handler, plus the optionality of `primary` in the stored entry types, so an unspecified Primary (and any other unspecified sub-attribute) is stored and serialized as ABSENT rather than coerced to the model default. The CreateUser decode path must use the same representation so create and update agree, and an explicitly supplied `Primary: false` must still round-trip as present-and-false.

Traces: identitystore.update-user-phone-numbers-full-replace.

## Evidence
Behavioral suite (simparity-behavioral-aws-v1) against the emulator: 82/85 passed; identitystore 82/83 (99%), 1 failed. The other two failures (sso-oidc CreateTokenWithIAM) are OWNED by an existing open issue and are not part of this one.

[mismatch] identitystore.update-user-phone-numbers-full-replace
  File "check_identitystore_update_user_phone_numbers_full_replace.py", line 51, in run
    assert phones[0].get("Primary") is not False, (
  AssertionError: the old element's Primary=False survived an update that supplied no Primary -- the target merged fields into the existing element instead of replacing it wholesale: {'Value': '832-555-0100', 'Primary': False}

Sequence (all calls HTTP 200):
  1. CreateUser with PhoneNumbers=[{Value, Type: 'work', Primary: false}]
  2. DescribeUser round-trips all three members (asserted earlier in the check and PASSING)
  3. UpdateUser Operations=[{AttributePath: 'phoneNumbers', AttributeValue: [{Value: <new number>}]}]
  4. DescribeUser -> expected the single element to carry ONLY Value (no Type, no Primary); observed {Value, Primary: false}.
  The assertions on list length (exactly 1), on Value being the new number, and on Type being dropped all passed; `Primary` is the sole diverging member. The concrete phone numbers ('555-010-0001', '832-555-0100') are the check's own values and are not requirements.

Model: smithy_query shape=PhoneNumber on com-amazonaws-identitystore-awsidentitystore.json -> `"Primary": {"target": "BooleanType", "default": false, "type": "boolean"}`; PhoneNumbers is a list with length min 1 max 1.

Code (current checkout): services/service-com-amazonaws-identitystore-awsidentitystore/src/ops/update_user.rs:388-397 (`"phonenumbers" => user.phone_numbers = entries(value, path, SINGLE_ENTRY_MAX, "PhoneNumber", CONTACT_MEMBERS, phone)?`), :434-445 (`fn flag` -> `None | Some(Document::Null) => Ok(false)`), :631-640 (`fn phone` -> `primary: flag(members.get("Primary").copied(), path)?`). The same coercion is in `email` (:619-628), `role` (:643-652), `address` (:655-669) and `photo` (:673-682).

## Steps to Reproduce
1. Boot the emulator from this checkout (identitystore is bootstrapped).
2. Obtain the account's IdentityStoreId via sso-admin ListInstances (create an instance first if none exists).
3. CreateUser(IdentityStoreId, UserName=<unique>, DisplayName=<unique>, Name={GivenName, FamilyName}, PhoneNumbers=[{Value: "555-010-0001", Type: "work", Primary: false}]).
4. DescribeUser(IdentityStoreId, UserId) -> the seeded element round-trips Value, Type and Primary=false (this part already holds).
5. UpdateUser(IdentityStoreId, UserId, Operations=[{AttributePath: "phoneNumbers", AttributeValue: [{Value: "832-555-0100"}]}]).
6. DescribeUser(IdentityStoreId, UserId) and inspect PhoneNumbers[0].
   Expected (AWS): exactly one element carrying only Value="832-555-0100" — no Type member, no Primary member.
   Observed: {'Value': '832-555-0100', 'Primary': False} — Primary materialized from the model default, which is indistinguishable from the pre-update element's Primary=false surviving a merge.
7. Control that must keep working after the fix: repeat step 5 with AttributeValue=[{Value: "832-555-0100", Primary: false}] -> DescribeUser must report Primary present and false.
