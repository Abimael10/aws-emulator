Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 20b40f42cd78b95666b130bae2475dd4b1dad646

# StartSnapshot inherits parent blocks at indexes outside the child snapshot's own volume extent

_Filed: 2026-11-19T11:44:42Z_
_Operation: StartSnapshot | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `20b40f42cd78b95666b130bae2475dd4b1dad646`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StartSnapshot` with a `ParentSnapshotId` copies EVERY block of the parent into the new snapshot without regard to the new snapshot's own `VolumeSize`. The crate has one rule for which block indexes a snapshot addresses — an index at or past `blocks_in_volume(volume_size)` names an offset OUTSIDE the volume, and `PutSnapshotBlock` refuses such a write with the modelled `ValidationException` (`INVALID_BLOCK`, "Block index N is beyond the volume size extent") — and the inherit path skips it.

The result is a stored snapshot whose own geometry contradicts its contents: when the child's `VolumeSize` is smaller than the parent's, the child is persisted holding blocks at indexes its reported `VolumeSize`/`BlockSize` do not address. `ListSnapshotBlocks` then reports that index (with a token) alongside `VolumeSize: 1`, `GetSnapshotBlock` serves its 512 KiB of data at a logical offset past the end of the volume, `ListChangedBlocks` reports it as a changed block, while `PutSnapshotBlock` against the SAME (snapshot, index) is refused as beyond the extent. One index of one snapshot is therefore readable and unwritable at the same time, and nothing in the crate reconciles the two paths (no extent filter on the copy, and no comparison of the child's `VolumeSize` with the parent's).

## Evidence

The rule, stated once in this crate — services/service-com-amazonaws-ebs-ebs/src/state.rs:55-61:
"/// Blocks in one GiB of volume.
///
/// `PutSnapshotBlockRequest$BlockIndex` documents \"A block index is a logical index in
/// units of 512 KiB blocks … (logical offset of data/524288)\", so a volume of N GiB has
/// exactly `N * BLOCKS_PER_GIB` addressable block indexes and an index at or past that
/// names an offset outside the volume.
pub const BLOCKS_PER_GIB: i64 = (1024 * 1024 * 1024) / (SNAPSHOT_BLOCK_SIZE_BYTES as i64);"

The sibling path that ENFORCES it — services/service-com-amazonaws-ebs-ebs/src/ops/put_snapshot_block.rs:325-334:
"        // `BlockIndex` is \"a logical index in units of 512 KiB blocks … (logical offset
        // of data/524288)\", so an index at or past the snapshot's own volume extent
        // names an offset outside the volume.
        let index_limit = blocks_in_volume(volume_size);
        if i64::from(input.block_index) >= index_limit {
            return Err(validation(
                \"INVALID_BLOCK\",
                block_index_beyond_volume_message(input.block_index),
            ));
        }"

The path that SKIPS it — services/service-com-amazonaws-ebs-ebs/src/ops/start_snapshot.rs:348 takes the parent's whole block map ("(Some(parent.blocks.clone()), parent.sse_type)") and start_snapshot.rs:373-389 inserts every entry into the child with no index test at all:
"        let mut blocks = BTreeMap::new();
        // The parent's map was cloned once above; its entries are MOVED into the child
        // here rather than cloned a second time.
        for (block_index, entry) in parent_blocks.into_iter().flatten() {
            let block_token = store.mint_block_token(&snapshot_id, block_index);
            ...
            blocks.insert(
                block_index,
                BlockEntry {
                    block_token,
                    written: false,
                    ..entry
                },
            );
        }"
The stored child then reports `volume_size: input.volume_size` (start_snapshot.rs:408) while holding those indexes.

Test coverage: tests/all/snapshot_block_loop.rs:1151-1219 (`a_child_snapshot_starts_from_a_copy_of_the_parent_blocks`) only ever starts the child with the SAME size as the parent ("let parent = write_snapshot(&router, \"parent\", 4, written)" at line 1157 vs "{ \"VolumeSize\": 4, \"ParentSnapshotId\": parent.clone() }" at line 1162), and tests/all/minted_query_tokens.rs:95-114 likewise reuses `fixture.volume_size` for the child — so no test pins a child smaller than its parent.

## Steps to Reproduce

1. POST /snapshots with {"VolumeSize": 2} to mint a parent snapshot (blocks_in_volume(2) = 4096 addressable indexes).
2. PUT /snapshots/{parent}/blocks/3000 with 524288 bytes, x-amz-Data-Length: 524288 and the matching x-amz-Checksum — accepted (3000 < 4096).
3. POST /snapshots/completion/{parent} with x-amz-ChangedBlocksCount: 1 so the parent is sealed `completed`.
4. POST /snapshots with {"VolumeSize": 1, "ParentSnapshotId": "<parent>"} — accepted 201; the child inherits the block at index 3000 even though blocks_in_volume(1) = 2048.
5. Seal the child (POST /snapshots/completion/{child} with x-amz-ChangedBlocksCount: 0) and GET /snapshots/{child}/blocks: the listing reports BlockIndex 3000 together with VolumeSize 1 and BlockSize 524288, and GET /snapshots/{child}/blocks/3000?blockToken=... returns the bytes.
6. PUT /snapshots/{child}/blocks/3000 on a pending child instead: refused HTTP 400 ValidationException INVALID_BLOCK "Block index 3000 is beyond the volume size extent" — the same index the read path serves.
