Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 864dd26a02416cc990250070e65c7b703d06755e

# The two listings implement the default page size and the page-token check twice

_Filed: 2026-11-19T10:21:54Z_
_Operation: ListSnapshotBlocks | Class: maintainability_

> _Note: this issue was identified against the code base at commit `864dd26a02416cc990250070e65c7b703d06755e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListSnapshotBlocks` and `ListChangedBlocks` each carry their own private copy of the same pagination rules, in a crate whose state module exists precisely to state a shared rule once: the `DEFAULT_MAX_RESULTS` constant with its identical justification comment, the identical page-size computation (including the `unwrap_or(0)` saturation branch), and the identical "decode the token, check it against what this snapshot reports, otherwise refuse with `INVALID_PAGE_TOKEN` and this sentence" block. The copies can drift: only `ListChangedBlocks`'s refusal is covered by a test, so a change to one copy would silently leave the other behind. `encode_page_token`/`decode_page_token` already live in `crate::state`, which is where the rest of this rule belongs; moving it there changes no response and no existing test.

## Evidence

Copy 1 — services/service-com-amazonaws-ebs-ebs/src/ops/list_snapshot_blocks.rs:104-109 and 207-208, 244-259:
"/// The page size used when the request names no `MaxResults`. ... const DEFAULT_MAX_RESULTS: i32 = 100;"
"        let page_size = usize::try_from(input.max_results.unwrap_or(DEFAULT_MAX_RESULTS))\n            .unwrap_or(0);"
"            Some(token) => decode_page_token(token)\n                .filter(...)\n                .map(|(_, next_index)| next_index)\n                .ok_or_else(|| {\n                    validation(\n                        \"INVALID_PAGE_TOKEN\",\n                        format!(\n                            \"NextToken is not a page token this service issued for snapshot {}\",\n                            input.snapshot_id\n                        ),\n                    )\n                })?,"

Copy 2 — services/service-com-amazonaws-ebs-ebs/src/ops/list_changed_blocks.rs:112-117 and 212-213, 291-308: the same doc comment and `const DEFAULT_MAX_RESULTS: i32 = 100;`, the same `let page_size = usize::try_from(input.max_results.unwrap_or(DEFAULT_MAX_RESULTS)).unwrap_or(0);`, and the same decode/filter/`ok_or_else(validation("INVALID_PAGE_TOKEN", format!("NextToken is not a page token this service issued for snapshot {}", ...)))` block.

The crate's own convention for a rule shared by several operations — services/service-com-amazonaws-ebs-ebs/src/state.rs:505-511: "A refusal's status and error code are always the contract. ... Those are the ones below, each stated once so every operation that raises the same refusal words it the same way" — and the page-token codec already follows it (state.rs:752-762, `encode_page_token`/`decode_page_token`).

Tests that cover the behaviour and must stay green unchanged: tests/all/changed_blocks_between_snapshots.rs:758 (the `INVALID_PAGE_TOKEN` refusal, ListChangedBlocks only), tests/all/minted_query_tokens.rs:274-300 (`a_minted_page_token_resumes_a_listing_unescaped`), tests/all/snapshot_block_loop.rs (ListSnapshotBlocks paging and `ExpiryTime`).

## Steps to Reproduce

1. Open services/service-com-amazonaws-ebs-ebs/src/ops/list_snapshot_blocks.rs:104-109 and services/service-com-amazonaws-ebs-ebs/src/ops/list_changed_blocks.rs:112-117 and compare the constant and its doc comment: byte-for-byte the same rule, declared twice.
2. Compare list_snapshot_blocks.rs:207-208 with list_changed_blocks.rs:212-213 (the same page-size computation) and list_snapshot_blocks.rs:244-259 with list_changed_blocks.rs:291-308 (the same token-decode/refusal, with the same message template).
3. Grep the test tree for `INVALID_PAGE_TOKEN`: only changed_blocks_between_snapshots.rs:758 matches, so the ListSnapshotBlocks copy of the refusal is unguarded and the two can diverge unnoticed.
