Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 13e968f2d712b86cbfec63dd795b2bc21b7f9b4c

# ListRules honours a forged NextToken naming a position only ANOTHER Region's endpoint reached, silently dropping that Region's rules

_Filed: 2026-11-16T11:52:48Z_
_Operation: ListRules | Class: pagination_

> _Note: this issue was identified against the code base at commit `13e968f2d712b86cbfec63dd795b2bc21b7f9b4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListRules` is supposed to refuse any `NextToken` that is not a cursor the endpoint being called handed out. The authenticity gate is `ComAmazonawsRbinAmazonrecyclebinState::page_cursor`, which asks two questions: does the cursor carry the Region this request is signed for, and does it name a position the store has minted (`has_minted_identifier`).

The second question is asked against a GLOBAL watermark, while the walk it guards is REGIONAL. `rules` is one map holding "Every retention rule of every Region, keyed by `Identifier`" and `mint_identifier` takes no Region at all, so the identifier space is shared: the position a `CreateRule` signed for Region A occupies is inside the watermark that `has_minted_identifier` answers `true` for when the request is signed for Region B.

A client can therefore mint a cursor of its OWN Region naming a position that Region's pager never emitted (one occupied by another Region's rule) and have it honoured. `page_from` then walks with `Bound::Excluded(<that identifier>)`, so every rule of the caller's Region sorting at or before it is dropped and the caller is answered HTTP 200 with a short (often empty) page and no `NextToken` — exactly the outcome `page_cursor`'s own doc says it refuses with `invalid_page_token`.

This is the Region-crossing residue of the authenticity gate: the shape-only acceptance and the all-zeros bottom end of the watermark were both closed, and every identifier-shaped value ABOVE the global watermark is still correctly refused. What is not refused is a value inside the global watermark but outside the calling Region's own walk.

## Evidence

The rule the crate states for itself — `src/state.rs:7-10` (module header):
```
//! order, and its `NextToken` is a cursor naming the last identifier it returned, so a
//! page boundary stays stable while rules are added or removed. A cursor is honoured
//! only by the endpoint that issued it and only for a position it really handed out
//! ([`ComAmazonawsRbinAmazonrecyclebinState::page_cursor`]).
```

and the same rule restated on the gate itself — `src/state.rs:1343-1351`:
```
    /// A cursor is honoured only when it is one of THIS endpoint's own: it has to decode
    /// to the cursor form [`encode_page_token`] renders, to have been issued by the
    /// Region this request is signed for, and to name a position this store has actually
    /// minted ([`Self::has_minted_identifier`]). Anything else is no cursor at all — a
    /// value a client invented that merely satisfies `NextToken`'s `@pattern`, or a
    /// cursor another Region's endpoint issued — and resuming "after" a position never
    /// handed out would answer a page that silently drops every rule sorting at or
    /// before it (an empty one, for a value sorting above them all), so the caller
    /// refuses it with [`invalid_page_token`] instead.
```

The gate that breaks it — `src/state.rs:1352-1356`:
```
    pub fn page_cursor(&self, region: &str, token: &str) -> Option<String> {
        let cursor = parse_page_token(token)?;
        (cursor.region == region && self.has_minted_identifier(&cursor.identifier))
            .then_some(cursor.identifier)
    }
```

The watermark it delegates the "really handed out" half to carries NO Region — `src/state.rs:1334-1338`:
```
    pub fn has_minted_identifier(&self, identifier: &str) -> bool {
        is_rule_identifier(identifier)
            && identifier > base62_identifier(INITIAL_IDENTIFIER_SEED).as_str()
            && identifier <= base62_identifier(self.next_identifier_seed).as_str()
    }
```

and the counter behind it is shared by every Region — `src/state.rs:1175-1179`:
```
    /// Every retention rule of every Region, keyed by `Identifier` and ordered so
    /// paging is stable. A rule belongs to exactly one Region ([`Rule::region`]), and
    /// the resolvers below are the only way a handler reaches one, so the key order is
    /// shared while the rules themselves stay partitioned.
    pub rules: BTreeMap<String, Rule>,
```
`src/state.rs:1200-1204`:
```
    pub fn mint_identifier(&mut self) -> String {
        loop {
            self.next_identifier_seed = self.next_identifier_seed.wrapping_add(1);
            let candidate = base62_identifier(self.next_identifier_seed);
```
and `src/ops/create_rule.rs:311` takes it with no Region argument:
```
        let identifier = store.mint_identifier();
```

Where the accepted position is used, over the Region-filtered range — `src/state.rs:1369-1376`:
```
        let lower = match after {
            Some(identifier) => Bound::Excluded(identifier.to_string()),
            None => Bound::Unbounded,
        };
        self.rules
            .range((lower, Bound::Unbounded))
            .filter(move |(_, rule)| rule.region == region)
```

The handler's own statement of what must happen instead — `src/ops/list_rules.rs:181-198`:
```
        // A `NextToken` this endpoint did not hand out resolves to no cursor at all:
        // only a cursor of THIS Region's walk, naming a position the store really
        // minted, says where to resume. Answering the first page instead would
        // silently restart the walk, and resuming after a position never handed out
        // would answer a page missing every rule that sorts at or before it.
        ...
                    return Err(ListRulesErrorModeled::ValidationException(
                        crate::state::invalid_page_token(),
                    )
                    .into());
```

Test coverage, and why the case slips through: `tests/all/list_rules_page_token_authenticity.rs:301` (`a_cursor_one_region_issued_is_no_cursor_of_another_regions_walk`) only replays the ISSUER's token — which carries the issuer's own Region string, so the `cursor.region == region` half catches it (line 338 `assert_refused(&router, region, resource_type, &cursor, &case)`), never a token re-encoded WITH the target Region's name. The same file's `list_rules_refuses_every_page_token_it_did_not_issue` builds `let router = router();` per Region (line 205), so no store it probes ever holds rules of two Regions; its `UNMINTED_IDENTIFIERS` (lines 49-55) are all asserted to "sort after every minted identifier" (line 257), i.e. above the global watermark. `tests/all/list_rules_minted_page_positions.rs:104` likewise creates a fresh `router()` for each Region before probing positions.

## Steps to Reproduce

1. Start the emulator with a fresh rbin store.
2. Sign a request for eu-central-1 and POST /rules with {"ResourceType":"EBS_SNAPSHOT","RetentionPeriod":{"RetentionPeriodValue":7,"RetentionPeriodUnit":"DAYS"}} — HTTP 201, Identifier is "00000000001" (the first global mint).
3. Sign a request for us-east-1 and POST /rules with the same body — HTTP 201, Identifier is "00000000002". This position was handed out by us-east-1's endpoint only; eu-central-1's pager never emitted it, because eu-central-1's pages report eu-central-1's rules alone.
4. Sign for eu-central-1 and POST /list-rules with {"ResourceType":"EBS_SNAPSHOT"} — HTTP 200, Rules reports "00000000001", no NextToken. This is eu-central-1's whole walk.
5. Sign for eu-central-1 and POST /list-rules with {"ResourceType":"EBS_SNAPSHOT","NextToken":"<base64 of \"eu-central-1/00000000002\">"} — a cursor naming the caller's own Region and a position that Region's endpoint never handed out.
6. Expected per src/state.rs:1343-1351 and src/ops/list_rules.rs:181-185: HTTP 400 ValidationException with Reason=INVALID_PAGE_TOKEN and no page in the body.
7. Actual: HTTP 200 with Rules=[] and no NextToken — page_cursor accepts the token because has_minted_identifier is answered off the global seed, and Bound::Excluded("00000000002") drops eu-central-1's rule "00000000001": the page silently loses every rule of the Region sorting at or before the forged position.
8. For contrast, replay the same request with "eu-central-1/ZZZZZZZZZZZ" (above the global watermark): correctly refused with 400 / INVALID_PAGE_TOKEN — showing the residue is positions inside the global watermark but outside the calling Region's own walk.
