// SPDX-License-Identifier: CC0-1.0

//! The BYTES one EBS volume holds for the guests it is attached to, as the device's own blocks.
//!
//! # Why a volume holds them
//!
//! A volume is attached in order to be WRITTEN, and what a write leaves is the VOLUME's: that is
//! what makes a read-back answer the bytes that were written, and what makes them follow the
//! volume when it is detached from one instance and attached to another
//! ([`super::guest_disk_io`], which performs those copies, and
//! [`super::VolumeRecord::guest_data`], which holds this for each volume).
//!
//! # Sparse, so a large device costs what was written
//!
//! Only the blocks something has WRITTEN are held, and every other byte of the device reads as
//! zero — which is what a block device does. So a guest addresses the whole of a large volume,
//! writes a payload wherever it likes in it, and this process holds only the bytes it really
//! wrote.
//!
//! # Bounds
//!
//! What ONE volume holds is bounded ([`MAX_VOLUME_GUEST_BYTES`]), and so is what the store holds
//! for every volume and machine at once
//! ([`super::guest_disk_io::MAX_GUEST_DATA_BYTES`], which a write is given as its `may_hold`):
//! the bytes are caller-supplied, so a write past either is a SHORT write — the answer a write
//! past the end of a device gets — rather than one that silently vanished.

use ::std::collections::BTreeMap;

/// How many bytes of its guest's data ONE volume holds.
///
/// The EMULATOR's own bound: the bytes come from caller-supplied text, so what one volume can make
/// this process hold is bounded here rather than by the volume's own size. Far above the few
/// mebibytes a script writes to check that a device carries the volume it meant, and far below what
/// would cost this process anything noticeable.
///
/// It is a bound on what is HELD and not on WHERE it may be held: the data of a volume is kept in
/// the blocks something has written ([`VolumeData`]), so a guest addresses the whole of a large
/// device and pays only for the bytes it really wrote.
pub const MAX_VOLUME_GUEST_BYTES: usize = 64 * 1024 * 1024;

/// How many bytes ONE block of a volume's data is ([`VolumeData`]).
///
/// The unit the sparse data is held in: a write of one byte holds one block, and a write of a
/// mebibyte holds one or two. The emulator's own number, far below the bound above and far above
/// the 512 bytes a copy moves by default, so a script's own blocks line up with these.
pub const DATA_BLOCK_BYTES: usize = 1024 * 1024;

/// The BYTES one volume holds for its guest, as the device's own blocks (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeData {
    /// The blocks something has written, by their index in the device.
    blocks: BTreeMap<usize, Vec<u8>>,
}

impl VolumeData {
    /// How many bytes this volume holds for its guest.
    #[must_use]
    pub fn held_bytes(&self) -> usize {
        self.blocks.len().saturating_mul(DATA_BLOCK_BYTES)
    }

    /// Whether the volume holds nothing at all, which is every volume nothing has written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// `length` bytes of the device from `offset`, as it answers them: what was written there, and
    /// zeroes for every byte of it nothing has written.
    #[must_use]
    pub fn read(&self, offset: usize, length: usize) -> Vec<u8> {
        let mut answered = vec![0_u8; length];
        for (index, block) in &self.blocks {
            let at = index.saturating_mul(DATA_BLOCK_BYTES);
            // Where this block and the range being read overlap.
            let from = at.max(offset);
            let until = at
                .saturating_add(block.len())
                .min(offset.saturating_add(length));
            if from >= until {
                continue;
            }
            let into = from.saturating_sub(offset);
            let taken = from.saturating_sub(at);
            answered[into..into.saturating_add(until - from)]
                .copy_from_slice(&block[taken..taken.saturating_add(until - from)]);
        }
        answered
    }

    /// Write `bytes` at `offset`, answering how many of them the volume took: all of them, or as
    /// many as it can hold before [`MAX_VOLUME_GUEST_BYTES`] and before `may_hold`, the bytes the
    /// store has room for — which is what makes a write past what a device can take a SHORT write
    /// rather than one that silently vanished.
    pub fn write(&mut self, offset: usize, bytes: &[u8], may_hold: usize) -> usize {
        let mut taken = 0_usize;
        let mut added = 0_usize;
        while taken < bytes.len() {
            let at = offset.saturating_add(taken);
            let index = at / DATA_BLOCK_BYTES;
            let within = at % DATA_BLOCK_BYTES;
            let room = DATA_BLOCK_BYTES - within;
            let writing = room.min(bytes.len() - taken);
            if !self.blocks.contains_key(&index)
                && (self.held_bytes().saturating_add(DATA_BLOCK_BYTES) > MAX_VOLUME_GUEST_BYTES
                    || added.saturating_add(DATA_BLOCK_BYTES) > may_hold)
            {
                // The volume holds as much as it can, or the store has no room for another block
                // of it: the rest of this write did not happen.
                return taken;
            }
            if !self.blocks.contains_key(&index) {
                added = added.saturating_add(DATA_BLOCK_BYTES);
            }
            let block = self
                .blocks
                .entry(index)
                .or_insert_with(|| vec![0_u8; DATA_BLOCK_BYTES]);
            block[within..within.saturating_add(writing)]
                .copy_from_slice(&bytes[taken..taken.saturating_add(writing)]);
            taken = taken.saturating_add(writing);
        }
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a read answers what was written at that range and zeroes for the rest of the device —
    /// over a table of offsets, the written range and the range read differing in each.
    #[test]
    fn a_read_answers_what_was_written_there_and_zeroes_elsewhere() {
        // (case, the offset written at, the offset read from)
        let rows = [
            ("the start of the device", 0_usize, 0_usize),
            ("inside the first block", 16, 0),
            ("one block in", DATA_BLOCK_BYTES, DATA_BLOCK_BYTES),
            ("far into the device", 4096 * DATA_BLOCK_BYTES, 4096 * DATA_BLOCK_BYTES),
        ];
        let payload = b"the bytes a guest wrote\n";
        for (case, written_at, read_from) in rows {
            let mut data = VolumeData::default();
            let taken = data.write(written_at, payload, MAX_VOLUME_GUEST_BYTES);
            assert_eq!(
                taken,
                payload.len(),
                "case {case:?}: expected the whole payload to land, got {taken}"
            );
            // Only the blocks the write touched are held, whatever the offset.
            assert_eq!(
                data.held_bytes(),
                DATA_BLOCK_BYTES,
                "case {case:?}: expected one block held, got {}",
                data.held_bytes()
            );
            // A read from the offset the case names, wide enough to cover the written range and
            // some of the device beyond it: the bytes written are there, at the offset they were
            // written at, and every other byte of the device reads as zero.
            let into = written_at.saturating_sub(read_from);
            let length = into.saturating_add(payload.len()).saturating_add(8);
            let answered = data.read(read_from, length);
            let mut expected = vec![0_u8; length];
            expected[into..into.saturating_add(payload.len())].copy_from_slice(payload);
            assert_eq!(
                answered, expected,
                "case {case:?}: expected the written bytes at the written range and zeroes \
                 elsewhere"
            );
        }
        assert!(
            VolumeData::default().is_empty(),
            "a volume nothing has written holds nothing"
        );
    }

    /// RULE: a write past what the volume may hold, or past what the store has room for, is SHORT.
    #[test]
    fn a_write_past_a_bound_is_short() {
        // (case, the bytes the store has room for, the bytes the write may leave)
        let rows = [
            ("room for one block", DATA_BLOCK_BYTES, DATA_BLOCK_BYTES),
            ("room for two", 2 * DATA_BLOCK_BYTES, 2 * DATA_BLOCK_BYTES),
            ("no room at all", 0, 0),
        ];
        let payload = vec![b'x'; 3 * DATA_BLOCK_BYTES];
        for (case, may_hold, expected) in rows {
            let mut data = VolumeData::default();
            let taken = data.write(0, &payload, may_hold);
            assert_eq!(
                taken, expected,
                "case {case:?}: expected {expected} bytes to land, got {taken}"
            );
            assert_eq!(
                data.held_bytes(),
                expected,
                "case {case:?}: expected the volume to hold exactly what landed"
            );
        }
    }
}
