Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 09d791ea5bf024b9268e3fbec7c7e0d5c86fb409

# GetRecord's unresolvable-feature-group ValidationError fills Message unbounded, though the modelled Message maximum is 2048 and BatchWriteRecord cuts the same text to that bound

_Filed: 2026-11-14T15:38:35Z_
_Operation: GetRecord | Class: error-response_

> _Note: this issue was identified against the code base at commit `09d791ea5bf024b9268e3fbec7c7e0d5c86fb409`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

When `GetRecord`'s `FeatureGroupName` resolves to no feature group, the handler answers the modelled `ValidationError` with `message: Some(message)` — the resolution message verbatim, with no length bound. That message is `crate::state::no_such_feature_group(arn)`, which embeds the fully qualified feature-group ARN built by `feature_group_arn(region, name)`; `region` comes from the request's own SigV4 credential scope through `crate::state::request_region`, which applies no length bound to that field at all (it only drops an EMPTY region). So the member this handler fills can exceed the maximum its own target shape declares: `ValidationError$Message` targets `Message`, modelled `length 0..2048`.

The crate already states and solves this exact rule one module over: `BatchWriteRecord` routes every per-entry message through `bounded_message`, cutting it at the last character boundary within `MAX_ERROR_MESSAGE = 2048` — and the text it bounds is produced by the SAME `crate::state::no_such_feature_group` helper (batch_write_record.rs:260-266). The read paths do not call it, so one operation answers the same refusal inside the modelled bound and its sibling outside it.

The same unbounded fill is in `ListRecords`' handler (list_records.rs:199-205), which fills the identical member from the identical helper, so a fix belongs in the shared refusal path rather than in one handler.

## Evidence

The modelled bound on the member the handler fills — `smithy_query(query="shape", shape="ValidationError")`:
```
"ValidationError": {"shape_id": "com.amazonaws.sagemakerfeaturestoreruntime#ValidationError", "type": "structure", "errorKind": "client", "httpError": 400, "members": {"Message": {"target": "Message", "length": {"min": 0, "max": 2048}}}}
```

The unbounded fill — services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/ops/get_record.rs:195-202:
```
            Err(unresolved) => {
                return Err(match unresolved {
                    crate::state::Unresolved::Refused(message) => {
                        GetRecordErrorModeled::ValidationError(ValidationError {
                            message: Some(message),
                        })
                        .into()
                    }
```

What that message carries — src/state.rs:783-785:
```
pub fn no_such_feature_group(arn: &str) -> String {
    format!("Resource Not Found: Amazon SageMaker can't find a FeatureGroup with name [{arn}].")
}
```
built from the ARN of the request's own region — src/state.rs:542-548:
```
pub fn feature_group_arn(region: &str, name_or_arn: &str) -> String {
    ::emulator_server::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("feature-group/{}", group_key(name_or_arn)),
    )
}
```
with that region read from the credential scope under no length bound — src/state.rs:527-532:
```
pub fn request_region(ctx: &smithy_server_runtime::OperationContext<'_>) -> String {
    ::emulator_server::sigv4_credential_scope(&ctx.parts().headers)
        .map(|scope| scope.region)
        .filter(|region| !region.is_empty())
        .unwrap_or_else(|| DEFAULT_REGION.to_owned())
}
```

The other side of the inconsistency, in this same crate — src/ops/batch_write_record.rs:385-404:
```
/// `message`, cut to the modelled maximum length of an `ErrorMessage`.
...
fn bounded_message(mut message: String) -> String {
    if message.len() > MAX_ERROR_MESSAGE {
...
/// The modelled maximum length of a `BatchWriteRecordError`'s `ErrorMessage`: its target
/// `Message` is `len 0..2048`
const MAX_ERROR_MESSAGE: usize = 2048;
```
and it is applied to the very same `no_such_feature_group` text at src/ops/batch_write_record.rs:260-266.

The same unbounded fill on the sibling read path — src/ops/list_records.rs:199-205:
```
                    crate::state::Unresolved::Refused(message) => {
                        ListRecordsErrorModeled::ValidationError(ValidationError {
                            message: Some(message),
                        })
                        .into()
                    }
```

Test coverage: tests/all/refusal_message_wording.rs:247-252 and 304-365 assert the EXACT refusal text for `GetRecord`/`ListRecords` against an unresolvable group, with regions `us-east-1`, `eu-west-1`, `ap-southeast-2`, `us-west-2` and a group name at its 64-character maximum — every one of those messages is far under 2048 characters, so cutting the message at 2048 leaves each of those assertions unchanged.

## Steps to Reproduce

1. Boot the emulator with this service mounted.
2. Create no feature group named `ghost` (the directory the data plane resolves against stays without it).
3. Send `GET /FeatureGroup/ghost?RecordIdentifierValueAsString=R1` with an `Authorization` header whose SigV4 credential scope names a very long region, e.g. `AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/<4000 'a' characters>/sagemaker/aws4_request, SignedHeaders=host, Signature=x` — `request_region` (src/state.rs:527-532) accepts any non-empty region.
4. Observe the 400 body: `Message` is the whole `Resource Not Found: Amazon SageMaker can't find a FeatureGroup with name [arn:aws:sagemaker:<4000 chars>:000000000000:feature-group/ghost].` string, i.e. roughly 4100 characters against the modelled `ValidationError$Message` maximum of 2048.
5. Send `POST /BatchWriteRecord` with `Entries=[{"FeatureGroupName":"ghost","Record":[{"FeatureName":"customer_id","ValueAsString":"C1"}]}]` and the same `Authorization` header: the per-entry `ErrorMessage` for the identical refusal is cut to 2048 characters (batch_write_record.rs:381, 390-399), so the two paths answer the same refusal in and out of the modelled bound.
6. Repeat step 3 as `POST /FeatureGroup/ghost/ListRecords` with body `{}` for the same overflow on the other read path (list_records.rs:199-205).
