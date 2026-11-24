// SPDX-License-Identifier: CC0-1.0

//! The RECORD BATCH itself: the bytes a `Produce` carries its records in and the bytes
//! a `Fetch` hands them back in (Apache Kafka's record batch, magic 2 — the format
//! every client since 0.11 writes).
//!
//! A record is not a key and a value: since KIP-82 it also carries HEADERS, an ordered
//! sequence of `(key, value)` pairs whose keys may REPEAT and whose values may be
//! absent, and since the batch carries a timestamp type it also says whose clock
//! stamped it. Order and bytes of those headers are part of the record a producer sent,
//! so the record layer is read and written against the format here, field by field,
//! rather than through a representation that keeps one value per header key — a map
//! cannot carry two headers of one name, and a batch decoded through one hands a
//! consumer back a record the producer did not send.
//!
//! Only the RECORD LAYER is read here. The compression of a batch is each codec's own
//! maintained crate, bounded as it runs ([`super::codecs`]), and a batch in one of the
//! two older formats, which have no headers at all, is decoded by the protocol
//! implementation ([`::kafka_protocol::records::RecordBatchDecoder`]), which is taken with
//! no codec of its own.

use ::bytes::{Buf, BufMut, Bytes, BytesMut};

use super::{codecs, RecordCodec, RecordHeaders, StoredRecord, SubmittedRecord};

/// The record batch format this endpoint writes and reads field by field: the one
/// Apache Kafka has used since 0.11, which is the only one that carries headers — and
/// the one every current producer writes, so it is also the format a batch this endpoint
/// is HANDED arrives in.
pub const MAGIC_V2: i8 = 2;

/// The two message formats that predate it, which a client fetching at an older version
/// of the API reads: the oldest, which carries neither a timestamp nor headers, and the
/// one that added a timestamp (Apache Kafka message format v0 and v1).
const MAGIC_V0: i8 = 0;
/// See [`MAGIC_V0`].
const MAGIC_V1: i8 = 1;

/// Where the magic byte sits in a batch: a batch opens with its base offset (8), its
/// length (4) and its partition leader epoch (4), and the magic follows them.
const MAGIC_OFFSET: usize = 16;

/// The bytes of one batch's fixed header that FOLLOW its `batch_length` field, which is
/// what that field counts: the leader epoch, the magic, the CRC, the attributes, the
/// last offset delta, the base and max timestamps, the producer id and epoch, the base
/// sequence and the record count.
const HEADER_BYTES_AFTER_LENGTH: usize = 49;
/// The whole fixed header, so a buffer shorter than this holds no batch.
const HEADER_BYTES: usize = HEADER_BYTES_AFTER_LENGTH + 12;

/// The attribute bits that name the batch's codec, and the bit that says the timestamps
/// in it are the ones the BROKER stamped (`LogAppendTime`) rather than the producer's.
const CODEC_MASK: i16 = 0x07;
/// See [`CODEC_MASK`].
const LOG_APPEND_TIME_BIT: i16 = 0x08;

/// The codec each value of those bits names (Apache Kafka record batch attributes).
const CODEC_NONE: i16 = 0;
/// See [`CODEC_NONE`].
const CODEC_GZIP: i16 = 1;
/// See [`CODEC_NONE`].
const CODEC_SNAPPY: i16 = 2;
/// See [`CODEC_NONE`].
const CODEC_LZ4: i16 = 3;
/// See [`CODEC_NONE`].
const CODEC_ZSTD: i16 = 4;

/// The timestamp a message in a format that carries none states, and the one a record
/// whose producer set none carries: Apache Kafka's `NO_TIMESTAMP`. A record that states it
/// is stamped by the broker at the instant it appends it.
const NO_TIMESTAMP: i64 = -1;

/// The `producer_id`, `producer_epoch` and `base_sequence` a batch no idempotent
/// producer owns carries (Apache Kafka writes -1 for each), each in the width its own
/// field has.
const NO_PRODUCER: i64 = -1;
/// See [`NO_PRODUCER`].
const NO_PRODUCER_EPOCH: i16 = -1;
/// See [`NO_PRODUCER`].
const NO_BASE_SEQUENCE: i32 = -1;

/// The most bytes of records ONE REQUEST may hold once they are decompressed, and the
/// most records one payload may carry. CHOSEN as process-safety bounds, not Apache Kafka
/// settings: a compressed batch is small on the wire and a codec expands it without a
/// limit of its own, so without a ceiling one frame would size the memory this decode
/// holds and the work it does.
///
/// What bounds this decode: the request frame itself ([`super::MAX_FRAME_BYTES`]), which
/// is what a connection may hand it at all; the bytes of records of the WHOLE REQUEST,
/// spent as ONE budget across every payload of it and every batch of those payloads, and
/// enforced INSIDE each decompression ([`codecs::decompress`]), so a batch is refused the
/// moment it would pass what is left rather than after it has been held; and the records
/// of one payload, past which nothing is read. Nothing is allocated from a count a batch
/// merely states.
pub const MAX_DECOMPRESSED_BYTES: usize = super::MAX_FRAME_BYTES;
/// See [`MAX_DECOMPRESSED_BYTES`].
const MAX_RECORDS_PER_PAYLOAD: usize = 100_000;

/// The records one produce payload carries, each carrying the codec of the batch it
/// arrived in, with what they decompressed to taken out of `budget` — the budget of the
/// REQUEST this payload is one entry of, so the entries of one frame cannot each spend a
/// ceiling of their own.
///
/// `None` is a payload this endpoint cannot READ — a batch whose framing, whose CRC or
/// whose record layout does not hold, or one that would decompress past what is left of
/// the budget — which is a produce refused as corrupt rather than reported as appended.
#[must_use]
pub fn decode(payload: &Bytes, budget: &mut usize) -> Option<Vec<SubmittedRecord>> {
    if payload.is_empty() {
        return Some(Vec::new());
    }
    // A batch in one of the two older formats has no headers to keep, so the protocol
    // implementation's own decoder reads it.
    if payload.len() <= MAGIC_OFFSET || payload[MAGIC_OFFSET] as i8 != MAGIC_V2 {
        return decode_legacy(payload, budget);
    }
    let mut buf = payload.clone();
    let mut records: Vec<SubmittedRecord> = Vec::new();
    // One payload may carry several batches, one after the other, which is what a
    // producer that did not flush between sends writes.
    while buf.remaining() >= HEADER_BYTES {
        records.extend(decode_batch(&mut buf, budget)?);
        if records.len() > MAX_RECORDS_PER_PAYLOAD {
            return None;
        }
    }
    // Trailing bytes that are too few to be a batch at all: a truncated payload, which
    // is exactly what the corrupt answer is for.
    match buf.has_remaining() {
        true => None,
        false => Some(records),
    }
}

/// The records of a batch in a format that predates headers, read field by field.
///
/// Those formats are a CONCATENATION of self-describing messages, each with its own
/// offset, size, checksum, magic and attributes. Only an UNCOMPRESSED message is read: a
/// message naming a codec would expand inside the decode, outside the budget every batch
/// above is read under. What is left is a message whose record is its own bytes, so the
/// budget is judged against the payload itself before anything is read and charged for it
/// after.
///
/// Neither format carries headers, and the OLDEST carries no timestamp at all: such a
/// message states [`NO_TIMESTAMP`], which is what the broker stamps at append.
fn decode_legacy(payload: &Bytes, budget: &mut usize) -> Option<Vec<SubmittedRecord>> {
    if payload.len() > *budget {
        return None;
    }
    let mut records: Vec<SubmittedRecord> = Vec::new();
    let mut remaining = payload.clone();
    while remaining.has_remaining() {
        let _offset = remaining.try_get_i64().ok()?;
        let size = usize::try_from(remaining.try_get_i32().ok()?).ok()?;
        if size > remaining.remaining() {
            return None;
        }
        let mut message = remaining.split_to(size);
        let stated_crc = message.try_get_u32().ok()?;
        // The two older formats checksum every MESSAGE on its own, over everything after
        // the checksum itself, with the CRC-32 Apache Kafka's message format states.
        if crc32(&message) != stated_crc {
            return None;
        }
        let magic = message.try_get_i8().ok()?;
        if magic != MAGIC_V0 && magic != MAGIC_V1 {
            return None;
        }
        let attributes = message.try_get_i8().ok()?;
        if i16::from(attributes) & CODEC_MASK != CODEC_NONE {
            return None;
        }
        let timestamp_ms = match magic >= MAGIC_V1 {
            true => message.try_get_i64().ok()?,
            false => NO_TIMESTAMP,
        };
        let key = read_sized_bytes(&mut message)?;
        let value = read_sized_bytes(&mut message)?;
        records.push(SubmittedRecord {
            timestamp_ms,
            key,
            value,
            headers: Vec::new(),
            codec: RecordCodec::None,
        });
        if records.len() > MAX_RECORDS_PER_PAYLOAD {
            return None;
        }
    }
    *budget = budget.checked_sub(payload.len())?;
    Some(records)
}

/// One `BYTES` of the two older message formats: an `INT32` length and that many bytes,
/// where `-1` is the ABSENCE of the field — a message with no key, a tombstone's absent
/// value.
fn read_sized_bytes(message: &mut Bytes) -> Option<Option<Vec<u8>>> {
    let length = message.try_get_i32().ok()?;
    if length < 0 {
        return Some(None);
    }
    let length = usize::try_from(length).ok()?;
    if length > message.remaining() {
        return None;
    }
    Some(Some(message.split_to(length).to_vec()))
}

/// The CRC-32 one message of those formats states over itself: the checksum Apache Kafka's
/// message format names, computed here because the record batch states a different one
/// (CRC-32C) and the gzip stream's own implementation is the only CRC-32 this crate links.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = ::flate2::Crc::new();
    crc.update(bytes);
    crc.sum()
}

/// One batch off the front of `buf`, leaving the buffer at the batch after it and taking
/// what its records decompressed to out of `budget`.
fn decode_batch(buf: &mut Bytes, budget: &mut usize) -> Option<Vec<SubmittedRecord>> {
    let _base_offset = buf.try_get_i64().ok()?;
    let batch_length = usize::try_from(buf.try_get_i32().ok()?).ok()?;
    if batch_length < HEADER_BYTES_AFTER_LENGTH || batch_length > buf.remaining() {
        return None;
    }
    // Everything the batch's CRC covers: the bytes from the attributes to its end, which
    // is the whole of it after the CRC field itself.
    let mut body = buf.split_to(batch_length);
    let _partition_leader_epoch = body.try_get_i32().ok()?;
    if body.try_get_i8().ok()? != MAGIC_V2 {
        return None;
    }
    let stated_crc = body.try_get_u32().ok()?;
    if ::crc32c::crc32c(&body) != stated_crc {
        return None;
    }
    let attributes = body.try_get_i16().ok()?;
    let _last_offset_delta = body.try_get_i32().ok()?;
    let base_timestamp = body.try_get_i64().ok()?;
    let _max_timestamp = body.try_get_i64().ok()?;
    let _producer_id = body.try_get_i64().ok()?;
    let _producer_epoch = body.try_get_i16().ok()?;
    let _base_sequence = body.try_get_i32().ok()?;
    let record_count = body.try_get_i32().ok()?;
    let codec = batch_codec(attributes)?;
    // The decompression is bounded by what is LEFT of the payload's budget, INSIDE the
    // decode itself, so a batch that would pass it is refused rather than held.
    let decompressed = codecs::decompress(&body, codec, *budget)?;
    *budget = budget.checked_sub(decompressed.len())?;
    let mut records = Bytes::from(decompressed);
    let count = usize::try_from(record_count).ok()?;
    if count > MAX_RECORDS_PER_PAYLOAD {
        return None;
    }
    // Nothing is allocated from the count the batch STATES: each record is pushed as it
    // is read, so a batch that states many records and carries none allocates nothing.
    let mut decoded = Vec::new();
    for _ in 0..count {
        decoded.push(decode_record(&mut records, base_timestamp, codec)?);
    }
    Some(decoded)
}

/// The codec the batch's attributes name, or `None` for a value that names no codec this
/// endpoint can read.
fn batch_codec(attributes: i16) -> Option<RecordCodec> {
    match attributes & CODEC_MASK {
        CODEC_NONE => Some(RecordCodec::None),
        CODEC_GZIP => Some(RecordCodec::Gzip),
        CODEC_SNAPPY => Some(RecordCodec::Snappy),
        CODEC_LZ4 => Some(RecordCodec::Lz4),
        CODEC_ZSTD => Some(RecordCodec::Zstd),
        _ => None,
    }
}

/// One record off the front of a batch's decompressed records, carrying the codec the
/// batch it came in was compressed with.
fn decode_record(
    buf: &mut Bytes,
    base_timestamp: i64,
    codec: RecordCodec,
) -> Option<SubmittedRecord> {
    let length = usize::try_from(read_varlong(buf)?).ok()?;
    if length > buf.remaining() {
        return None;
    }
    let mut record = buf.split_to(length);
    let _attributes = record.try_get_i8().ok()?;
    let timestamp_delta = read_varlong(&mut record)?;
    let _offset_delta = read_varlong(&mut record)?;
    let key = read_nullable_bytes(&mut record)?;
    let value = read_nullable_bytes(&mut record)?;
    let header_count = read_varlong(&mut record)?;
    let header_count = usize::try_from(header_count).ok()?;
    // Nothing is reserved from the count the record STATES, for the reason its records
    // are not: each header is pushed as it is read out of the bytes the budget was spent
    // on, so a record that states many headers and carries none holds nothing.
    let mut headers: RecordHeaders = Vec::new();
    for _ in 0..header_count {
        let name = read_nullable_bytes(&mut record)?.unwrap_or_default();
        let name = String::from_utf8(name).ok()?;
        headers.push((name, read_nullable_bytes(&mut record)?));
    }
    Some(SubmittedRecord {
        timestamp_ms: base_timestamp.saturating_add(timestamp_delta),
        key,
        value,
        headers,
        codec,
    })
}

/// A length-prefixed byte string of the record layer, where a length of -1 is the
/// ABSENCE of the field — a record with no key, a tombstone's absent value, a header
/// whose value is null — which is not the same as an empty one.
fn read_nullable_bytes(buf: &mut Bytes) -> Option<Option<Vec<u8>>> {
    let length = read_varlong(buf)?;
    if length < 0 {
        return Some(None);
    }
    let length = usize::try_from(length).ok()?;
    if length > buf.remaining() {
        return None;
    }
    Some(Some(buf.split_to(length).to_vec()))
}

/// One zig-zag varint of the record layer (Apache Kafka writes the record layer's
/// integers as Protocol Buffers varints over a zig-zag encoding).
fn read_varlong(buf: &mut Bytes) -> Option<i64> {
    let mut shift = 0u32;
    let mut raw = 0u64;
    loop {
        let byte = buf.try_get_u8().ok()?;
        raw |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
    // Zig-zag: the low bit carries the sign.
    Some(((raw >> 1) as i64) ^ -((raw & 1) as i64))
}

/// Write one zig-zag varint.
///
/// The shift is taken on the UNSIGNED value: zig-zag moves the sign into the low bit, so
/// the top bit is meant to be discarded, and a delta near the ends of the range would
/// otherwise overflow a signed shift.
fn write_varlong(buf: &mut BytesMut, value: i64) {
    let mut raw = ((value as u64) << 1) ^ ((value >> 63) as u64);
    while raw >= 0x80 {
        buf.put_u8((raw as u8 & 0x7f) | 0x80);
        raw >>= 7;
    }
    buf.put_u8(raw as u8);
}

/// Write one length-prefixed byte string, with -1 for an absent one.
fn write_nullable_bytes(buf: &mut BytesMut, value: Option<&[u8]>) {
    match value {
        None => write_varlong(buf, -1),
        Some(bytes) => {
            write_varlong(buf, i64::try_from(bytes.len()).unwrap_or(i64::MAX));
            buf.put_slice(bytes);
        }
    }
}

/// The batch a fetch answers `records` with, compressed with `codec`, in the MESSAGE
/// FORMAT the version of the fetch carries ([`fetch_magic`]).
///
/// Every field of each record is written back as the log holds it — its offset, its
/// timestamp and the kind of timestamp it is, its key, its value and its headers in
/// order — so a consumer reads the record the producer sent.
///
/// The two older formats carry neither headers nor (for the oldest) a timestamp, so a
/// client that negotiated one of them is answered with what that format HAS: a broker
/// converts the batch down to the format the fetching client can read, and what the
/// format has no field for cannot be reported in it. Those formats are written by the
/// protocol implementation's own encoder, which is what computes their per-message
/// checksum, and they are handed back UNCOMPRESSED — the codec of such a message is its
/// own, and every client of those versions reads an uncompressed one.
#[must_use]
pub fn encode(records: &[StoredRecord], codec: RecordCodec, magic: i8) -> Option<Bytes> {
    if magic < MAGIC_V2 {
        return encode_legacy(records, magic);
    }
    let first = records.first()?;
    let base_offset = first.offset;
    let base_timestamp = first.timestamp_ms;
    let last = records.last()?;
    let max_timestamp = records
        .iter()
        .map(|record| record.timestamp_ms)
        .max()
        .unwrap_or(base_timestamp);
    let mut raw = BytesMut::new();
    for record in records {
        let mut encoded = BytesMut::new();
        encoded.put_i8(0);
        write_varlong(&mut encoded, record.timestamp_ms.saturating_sub(base_timestamp));
        write_varlong(&mut encoded, record.offset.saturating_sub(base_offset));
        write_nullable_bytes(&mut encoded, record.key.as_deref());
        write_nullable_bytes(&mut encoded, record.value.as_deref());
        write_varlong(
            &mut encoded,
            i64::try_from(record.headers.len()).unwrap_or(i64::MAX),
        );
        for (name, value) in &record.headers {
            write_nullable_bytes(&mut encoded, Some(name.as_bytes()));
            write_nullable_bytes(&mut encoded, value.as_deref());
        }
        write_varlong(&mut raw, i64::try_from(encoded.len()).unwrap_or(i64::MAX));
        raw.put_slice(&encoded);
    }
    let compressed = codecs::compress(&raw, codec)?;
    let attributes = codec_bits(codec)
        | match first.log_append_time {
            true => LOG_APPEND_TIME_BIT,
            false => 0,
        };
    // Everything the CRC covers, built first: the attributes to the end of the records.
    let mut body = BytesMut::new();
    body.put_i16(attributes);
    body.put_i32(i32::try_from(last.offset.saturating_sub(base_offset)).unwrap_or(i32::MAX));
    body.put_i64(base_timestamp);
    body.put_i64(max_timestamp);
    body.put_i64(NO_PRODUCER);
    body.put_i16(NO_PRODUCER_EPOCH);
    body.put_i32(NO_BASE_SEQUENCE);
    body.put_i32(i32::try_from(records.len()).unwrap_or(i32::MAX));
    body.put_slice(&compressed);
    let mut batch = BytesMut::new();
    batch.put_i64(base_offset);
    // The length counts every byte after it: the leader epoch, the magic, the CRC and
    // the body.
    batch.put_i32(i32::try_from(body.len() + 9).unwrap_or(i32::MAX));
    batch.put_i32(0);
    batch.put_i8(MAGIC_V2);
    batch.put_u32(::crc32c::crc32c(&body));
    batch.put_slice(&body);
    Some(batch.freeze())
}

/// The attribute bits one codec is named by.
fn codec_bits(codec: RecordCodec) -> i16 {
    match codec {
        RecordCodec::None => CODEC_NONE,
        RecordCodec::Gzip => CODEC_GZIP,
        RecordCodec::Snappy => CODEC_SNAPPY,
        RecordCodec::Lz4 => CODEC_LZ4,
        RecordCodec::Zstd => CODEC_ZSTD,
    }
}

/// The MESSAGE FORMAT a `Fetch` of `api_version` is answered in: the magic a client that
/// negotiated that version can read.
///
/// A broker converts the records of its log DOWN to the format the fetching client
/// understands, and which that is follows from the version of the fetch alone: the record
/// batch (magic 2) arrived with `Fetch` version 4, the message set carrying a timestamp
/// (magic 1) with version 2, and version 0 and 1 of the API carry only the oldest message
/// set (magic 0) — a client that negotiated one of them cannot read a newer batch at all,
/// so answering one leaves it reading no records out of a non-empty answer.
#[must_use]
pub fn fetch_magic(api_version: i16) -> i8 {
    match api_version {
        ..=1 => MAGIC_V0,
        2..=3 => MAGIC_V1,
        _ => MAGIC_V2,
    }
}

/// The message set `records` are handed back in for a client that negotiated one of the
/// two formats that predate the record batch.
///
/// Those formats are a concatenation of self-describing messages: each one carries its own
/// offset, its size, its CRC-32 over everything after it, the magic, the attributes, the
/// timestamp (from magic 1 on) and its key and value. What the format has no field for is
/// not reported in it — magic 0 carries no timestamp, and neither format carries headers —
/// and the messages are handed back UNCOMPRESSED, which every client of those versions
/// reads.
fn encode_legacy(records: &[StoredRecord], magic: i8) -> Option<Bytes> {
    let mut encoded = BytesMut::new();
    for record in records {
        // Everything the message's CRC covers: the magic to the end of the value.
        let mut message = BytesMut::new();
        message.put_i8(magic);
        message.put_i8(match record.log_append_time {
            true => LOG_APPEND_TIME_BIT as i8,
            false => 0,
        });
        if magic >= MAGIC_V1 {
            message.put_i64(record.timestamp_ms);
        }
        write_sized_bytes(&mut message, record.key.as_deref());
        write_sized_bytes(&mut message, record.value.as_deref());
        encoded.put_i64(record.offset);
        // The size counts the CRC and everything it covers.
        encoded.put_i32(i32::try_from(message.len() + 4).unwrap_or(i32::MAX));
        encoded.put_u32(crc32(&message));
        encoded.put_slice(&message);
    }
    Some(encoded.freeze())
}

/// Write one `BYTES` of those formats, with `-1` for an absent one.
fn write_sized_bytes(message: &mut BytesMut, value: Option<&[u8]>) {
    match value {
        None => message.put_i32(-1),
        Some(bytes) => {
            message.put_i32(i32::try_from(bytes.len()).unwrap_or(i32::MAX));
            message.put_slice(bytes);
        }
    }
}
