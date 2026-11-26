Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: b453fe5364a6f110c9233c398905eb7b0cb9c03a

# The activation Handshake doc says a handshake carries a "non-empty" activationRegion, while the code under it claims an empty one and a sibling doc plus a test require that

_Filed: 2026-11-26T16:32:44Z_
_Operation: CreateGateway | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `b453fe5364a6f110c9233c398905eb7b0cb9c03a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateGateway` only redeems an `ActivationKey` this service issued through the activation handshake in `src/service/activation.rs`, so what that module CLAIMS decides which pre-authentication requests are answered at all.

The type-level doc of `Handshake` — the first thing a reader of that module sees, and the definition of what a handshake IS — states that a handshake is "a `GET` of the endpoint root carrying a non-empty [`ACTIVATION_REGION_PARAM`]". The constructor under it does not test for non-emptiness: `Handshake::of` accepts whatever `query_param` returns, so `?activationRegion=` (the parameter present with an empty value) IS claimed as a handshake, and `Handshake::of`'s own doc 30 lines below says so in the opposite words — "A handshake carrying the parameter with an EMPTY value is still a handshake: it is claimed here and refused by [`Self::names_a_region`] ... rather than one of them falling through to the front door's `404 UnknownService`".

The code is the half this crate pins: `tests/all/gateway_activation.rs` drives `activationRegion=` over the wire and asserts HTTP 400 naming "letters, digits and hyphens", which is only reachable because the empty value is claimed and then refused by `names_a_region`. So the defect is the stale type-level sentence. A reader who trusts it would add the `is_empty` guard it describes to `Handshake::of`; the request would then be claimed by nobody, the router would answer the front door's `404 UnknownService` for the very endpoint an appliance was told to ask a key of, and the pinned "an empty region" row would break. The sentence should describe the rule the constructor really applies (the parameter present, whatever its value), as the constructor's own doc already does.

## Evidence

The stale definition — `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/service/activation.rs:101-106`:
```
/// The handshake a request makes, or `None` for a request that is not one.
///
/// A handshake is a `GET` of the endpoint root carrying a non-empty
/// [`ACTIVATION_REGION_PARAM`]: the method and the parameter together are what separate it
/// from every modelled request of this service, which are `POST`s carrying an
/// `X-Amz-Target` and no such parameter.
```

The constructor under it, which applies no such test — same file `:139-147`:
```
    fn of(method: &str, path: &str, query: &str) -> Option<Self> {
        if !method.eq_ignore_ascii_case(http::Method::GET.as_str()) || path != ACTIVATION_PATH {
            return None;
        }
        Some(Self {
            region: query_param(query, ACTIVATION_REGION_PARAM)?,
            gateway_type: query_param(query, GATEWAY_TYPE_PARAM),
        })
    }
```
where `query_param` returns the decoded value as the query spells it — same file `:194-199`:
```
fn query_param(query: &str, name: &str) -> Option<String> {
    ::aws_smithy_schema_server::query::parse_form_urlencoded(query.as_bytes())
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}
```

The same file saying the opposite, directly above that constructor — `:135-138`:
```
    /// A handshake carrying the parameter with an EMPTY value is still a handshake: it is
    /// claimed here and refused by [`Self::names_a_region`], so every value this service
    /// cannot issue a key for is answered the same way, rather than one of them falling
    /// through to the front door's `404 UnknownService`.
```
and the refusal that case takes — `:154-156` and `:232-240`:
```
    fn names_a_region(&self) -> bool {
        crate::state::arn_region(&self.region) == self.region
    }
```
```
    if !handshake.names_a_region() {
        ...
        return text_response(400, invalid_region_message());
    }
```

Test coverage that pins the code side (so a "fix" toward the stale sentence would break it) — `tests/all/gateway_activation.rs:224-228`:
```
        (
            "an empty region",
            "activationRegion=".to_string(),
            "letters, digits and hyphens",
        ),
```
asserted at the same file `:231-241`:
```
    for (case, query, named) in &rows {
        let (status, body) = activation_handshake(&router, query).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a handshake this service cannot serve must be refused, got \
             {status} {body:?}"
        );
```

## Steps to Reproduce

1. Read `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/service/activation.rs:101-106`: the `Handshake` type doc defines a handshake as a `GET` of the endpoint root carrying a NON-EMPTY `activationRegion`.
2. Read `Handshake::of` at `:139-147` and `query_param` at `:194-199`: the only tests applied are the method and the path; the parameter's value is taken as spelled, so `?activationRegion=` yields `Some(Handshake { region: "" , .. })` and the request IS claimed.
3. Read `:135-138` (the constructor's own doc) and `:154-156`/`:232-240`: the crate states that an empty value is still a handshake and is refused by `names_a_region` with HTTP 400, explicitly so it does not fall through to the front door's `404 UnknownService`.
4. Confirm the behaviour is pinned: `tests/all/gateway_activation.rs:224-228` drives `activationRegion=` and `:231-241` asserts HTTP 400 whose body names "letters, digits and hyphens" — the `invalid_region_message()` text, reachable only if the request was claimed.
5. Note that adding the non-emptiness test the type doc describes would make `claims_parts`/`claims_request` reject that request, changing the answer from the pinned 400 to the router's 404 UnknownService — so `:103-106` is the stale half and is what should be reworded.
