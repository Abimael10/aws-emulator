// SPDX-License-Identifier: CC0-1.0

//! The CODECS a record batch's records may travel in, and the BOUND each one is read
//! under.
//!
//! A `Produce` names the codec itself in the batch it sends, so which decoder runs over
//! a connection's bytes is the caller's choice and the size of what that decoder produces
//! is not a function of what it was given: a small compressed batch can expand without
//! limit. So every decode here is bounded as it RUNS — the decoder is read through a
//! limited reader (or, where the format declares its own output length, that length is
//! read first) and the batch is refused the moment the budget would be crossed, before
//! the bytes are held. The bound is the caller's, passed in by the record layer, which
//! spends ONE budget across every batch of one payload.
//!
//! Each codec is the maintained crate for that compression, and each is PURE RUST — this
//! endpoint links no native compression library: `flate2` reads and writes the gzip
//! stream, `lz4_flex` the LZ4 frame, `ruzstd` the Zstandard frame, and `snap` the raw
//! snappy blocks that Apache Kafka's snappy batches carry inside the framing below.

use std::io::{Read, Write};

use super::RecordCodec;

/// The framing Apache Kafka wraps its snappy blocks in (the "xerial" stream every Kafka
/// client writes): a magic byte and `SNAPPY`, a zero, then the stream's version and the
/// minimum version compatible with it, each a big-endian `int32`, followed by the blocks
/// themselves — each a big-endian `int32` length and that many bytes of raw snappy.
const XERIAL_HEADER: [u8; 16] = [
    0x82, b'S', b'N', b'A', b'P', b'P', b'Y', 0, 0, 0, 0, 1, 0, 0, 0, 1,
];

/// How many bytes of records go into ONE snappy block of that stream: Apache Kafka's own
/// clients block at 32 KiB, so a batch this endpoint writes is framed the way a batch it
/// reads is.
const SNAPPY_BLOCK_BYTES: usize = 32 * 1024;

/// The records of one batch, decompressed with the codec the batch names — or `None`
/// when the bytes are not that codec's, or when what they decompress to would pass
/// `limit`, which is a batch this endpoint refuses rather than holds.
#[must_use]
pub fn decompress(compressed: &[u8], codec: RecordCodec, limit: usize) -> Option<Vec<u8>> {
    match codec {
        RecordCodec::None => (compressed.len() <= limit).then(|| compressed.to_vec()),
        RecordCodec::Gzip => read_bounded(::flate2::read::GzDecoder::new(compressed), limit),
        RecordCodec::Snappy => snappy_decompress(compressed, limit),
        RecordCodec::Lz4 => read_bounded(
            ::lz4_flex::frame::FrameDecoder::new(compressed),
            limit,
        ),
        RecordCodec::Zstd => read_bounded(
            ::ruzstd::decoding::StreamingDecoder::new(compressed).ok()?,
            limit,
        ),
    }
}

/// The records of one batch, compressed with `codec` for a fetch to answer with.
#[must_use]
pub fn compress(raw: &[u8], codec: RecordCodec) -> Option<Vec<u8>> {
    match codec {
        RecordCodec::None => Some(raw.to_vec()),
        RecordCodec::Gzip => {
            let mut encoder = ::flate2::write::GzEncoder::new(
                Vec::new(),
                ::flate2::Compression::default(),
            );
            encoder.write_all(raw).ok()?;
            encoder.finish().ok()
        }
        RecordCodec::Snappy => snappy_compress(raw),
        RecordCodec::Lz4 => {
            let mut encoder = ::lz4_flex::frame::FrameEncoder::new(Vec::new());
            encoder.write_all(raw).ok()?;
            encoder.finish().ok()
        }
        RecordCodec::Zstd => Some(::ruzstd::encoding::compress_to_vec(
            raw,
            ::ruzstd::encoding::CompressionLevel::Fastest,
        )),
    }
}

/// At most `limit` bytes out of `reader`, and `None` past that.
///
/// The bound is applied as the decoder PRODUCES its output — the reader stops one byte
/// past the budget — so what a decode may hold is this budget rather than whatever its
/// input expands to.
fn read_bounded<R: Read>(reader: R, limit: usize) -> Option<Vec<u8>> {
    let bound = u64::try_from(limit).ok()?.saturating_add(1);
    let mut decoded = Vec::new();
    reader.take(bound).read_to_end(&mut decoded).ok()?;
    (decoded.len() <= limit).then_some(decoded)
}

/// The records of a snappy batch: the blocks of its framing, each one's decompressed
/// length read from the block itself BEFORE it is decompressed, so a block that would
/// pass the budget is refused rather than expanded. A batch carrying one raw snappy block
/// with no framing at all is read the same way, which is the other form a client may send.
fn snappy_decompress(compressed: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut decoder = ::snap::raw::Decoder::new();
    let Some(mut blocks) = compressed.strip_prefix(&XERIAL_HEADER[..]) else {
        let declared = ::snap::raw::decompress_len(compressed).ok()?;
        if declared > limit {
            return None;
        }
        return decoder.decompress_vec(compressed).ok();
    };
    let mut decoded: Vec<u8> = Vec::new();
    while !blocks.is_empty() {
        let (length, rest) = blocks.split_at_checked(4)?;
        let length = usize::try_from(u32::from_be_bytes([
            length[0], length[1], length[2], length[3],
        ]))
        .ok()?;
        let (block, rest) = rest.split_at_checked(length)?;
        let declared = ::snap::raw::decompress_len(block).ok()?;
        if decoded.len().saturating_add(declared) > limit {
            return None;
        }
        decoded.extend_from_slice(&decoder.decompress_vec(block).ok()?);
        blocks = rest;
    }
    Some(decoded)
}

/// The same framing, written: the stream's header and then one block per
/// [`SNAPPY_BLOCK_BYTES`] of records.
fn snappy_compress(raw: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = ::snap::raw::Encoder::new();
    let mut out = XERIAL_HEADER.to_vec();
    for chunk in raw.chunks(SNAPPY_BLOCK_BYTES) {
        let block = encoder.compress_vec(chunk).ok()?;
        out.extend_from_slice(&u32::try_from(block.len()).ok()?.to_be_bytes());
        out.extend_from_slice(&block);
    }
    Some(out)
}
