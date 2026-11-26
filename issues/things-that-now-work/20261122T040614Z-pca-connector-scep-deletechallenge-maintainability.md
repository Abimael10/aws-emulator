Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 861999866ea9ac28e07c2c6c1f5e841f1e39e080

# Fold the four hand-written Unit-output schema blocks into one; the copies have already drifted

_Filed: 2026-11-22T04:06:14Z_
_Operation: DeleteChallenge | Class: maintainability_

> _Note: this issue was identified against the code base at commit `861999866ea9ac28e07c2c6c1f5e841f1e39e080`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Four of this crate's handlers hand-write the SAME unit-output schema rule inline instead of sharing one spelling: a `#[derive(Debug, Clone, Default)] pub struct <Op>Output;`, a `pub const SCHEMA` built as `Schema::new_struct(ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"), ShapeType::Structure, &[])` carried by `.with_http(HttpTrait::new(<method>, <uri>, Some(<status>)))`, a `pub static <OP>_OUTPUT_SCHEMA` pointing at it, and an empty `SerializableStruct::serialize_members` returning `Ok(())`. The four copies differ ONLY in the three HttpTrait arguments (method, URI, status); everything else, comment included, is byte-identical.

The duplication has already produced a drift inside this crate: the explanatory comment above the `SCHEMA` const was copied verbatim into all four, so `delete_challenge.rs` and `delete_connector.rs` now carry a comment that states a modelled status their own code contradicts — the comment says `204` while the `with_http` two lines below it, and the operation's own `SUCCESS_STATUS`, say `202`. That is exactly the drift a single shared spelling (one macro or one helper taking method/uri/status) would make impossible.

Responses are unaffected: the fix is to derive the four blocks from one place, keeping the same three HttpTrait arguments per operation, so every status and body on the wire stays what the existing tests already pin.

## Evidence

Copy 1 — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/delete_challenge.rs:90-108
```
#[derive(Debug, Clone, Default)]
pub struct DeleteChallengeOutput;

impl DeleteChallengeOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    )
    .with_http(::aws_smithy_schema::traits::HttpTrait::new(
        "DELETE",
        "/challenges/{ChallengeArn}",
        Some(202u16),
    ));
}

pub static DELETE_CHALLENGE_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &DeleteChallengeOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeleteChallengeOutput {
    fn serialize_members(&self, _s: &mut dyn ...ShapeSerializer) -> Result<(), ...SerdeError> { Ok(()) }
}
```
The same file's handler states the real status at delete_challenge.rs:171: `const SUCCESS_STATUS: u16 = 202u16;` — so the comment's "a modelled 204" is contradicted by the code it sits on top of.

Copy 2 — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/delete_connector.rs:90-112, identical block with the same wrong comment:
```
impl DeleteConnectorOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
```
against delete_connector.rs:101-105 `"DELETE", "/connectors/{ConnectorArn}", Some(202u16),` and delete_connector.rs:174 `const SUCCESS_STATUS: u16 = 202u16;`.

Copy 3 — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/tag_resource.rs:97-118 (`"POST", "/tags/{ResourceArn}", Some(204u16)`), where the shared comment is correct.
Copy 4 — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/untag_resource.rs:91-113 (`"DELETE", "/tags/{ResourceArn}", Some(204u16)`), likewise.

OPERATIONS.md:42 and :104 confirm which status belongs where: `## UntagResource` — `**Route:** DELETE /tags/{ResourceArn} -> 204`; `## DeleteConnector` — `**Route:** DELETE /connectors/{ConnectorArn} -> 202`.

Test coverage that must stay unchanged (and that already proves the code's statuses, not the comment's):
- tests/all/missing_resource_status_class.rs:8-12 — "`DeleteConnector` and `DeleteChallenge` are modelled with output `Unit` and http code 202".
- tests/all/tag_resource.rs:411-412 — `tagged.status, 204, "case {case_name:?}: expected the modelled 204, ..."`; tag_resource.rs:589-595 asserts `202` for DeleteChallenge and DeleteConnector.
- tests/all/untag_resource.rs:469-470 and :596-597 — `expected the modelled 204` and `expected 204 from the repeated call`.

## Steps to Reproduce

1. Open services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/delete_challenge.rs and read lines 90-108: a hand-written `DeleteChallengeOutput` with a `SCHEMA` const, a `DELETE_CHALLENGE_OUTPUT_SCHEMA` static and an empty `SerializableStruct` impl.
2. Open src/ops/delete_connector.rs:90-112, src/ops/tag_resource.rs:97-118 and src/ops/untag_resource.rs:91-113 — the same four-part block three more times, differing only in the method/URI/status handed to `HttpTrait::new`.
3. Compare the comment copied into all four ("so a modelled 204 answers 200") with the status each file actually declares: `Some(202u16)` / `SUCCESS_STATUS = 202u16` in the two delete files, `Some(204u16)` in the two tagging files — the comment is only true for two of the four copies.
4. Confirm the wire statuses the tests pin: `cargo test -p service-com-amazonaws-pcaconnectorscep-pcaconnectorscep` (missing_resource_status_class asserts 202 for both deletes; tag_resource/untag_resource assert 204), so collapsing the four copies into one spelling must leave every one of those assertions passing.
