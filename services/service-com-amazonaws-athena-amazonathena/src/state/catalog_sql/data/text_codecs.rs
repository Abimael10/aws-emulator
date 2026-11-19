// SPDX-License-Identifier: CC0-1.0

//! The COMPRESSION one TEXT object at a table's location carries, taken from its key.
//!
//! An object of a table whose objects are text is handed to a DECOMPRESSOR before any SerDe
//! reads it, and which decompressor is decided by the object's own NAME: the codec factory
//! Hadoop reads such a table through holds one codec per file extension, so an object keyed
//! `*.gz` is a gzip stream and one keyed `*.bz2` a bzip2 stream, and it is what the codec
//! produced — never the stored bytes — that the SerDe then splits into records by its
//! delimiter, its quoting and the header lines the table skips. An object whose key ends in
//! none of those extensions holds its records as the bytes it stores.
//!
//! What a codec produces is the maintained decoder's own output: no compressed format's
//! bytes are decoded here, only the lengths the two BLOCK-framed codecs write between their
//! chunks ([`read_framed_blocks`]). The extension decides the codec and the bytes never do,
//! so an object whose key names a codec its content is not is a split that cannot be opened
//! rather than bytes read as text.

use ::std::io::Read;

/// A compression one text object can carry: one variant per extension the codec factory
/// holds a codec under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// The gzip stream `.gz` names.
    Gzip,
    /// The zlib stream Hadoop's default codec writes, which `.deflate` names.
    Deflate,
    /// The bzip2 stream `.bz2` names.
    Bzip2,
    /// The Zstandard stream `.zst` names.
    Zstandard,
    /// Blocks of raw Snappy, framed the way Hadoop's block codecs frame them (`.snappy`).
    Snappy,
    /// Blocks of raw LZ4, framed the same way (`.lz4`).
    Lz4,
    /// The LZO compression `.lzo` names, whose decoder is a native library this emulator
    /// holds none of — so such an object is REPORTED rather than read as the bytes it
    /// stores.
    Lzo,
}

/// Every extension the codec factory recognises, with the codec it names.
///
/// The extensions are the ones Hadoop's own codec for each compression writes, which are
/// also the compressions Athena documents for text data: gzip, bzip2, deflate/zlib, Snappy,
/// LZ4, LZO and Zstandard.
const EXTENSIONS: &[(&str, Codec)] = &[
    (".gz", Codec::Gzip),
    (".deflate", Codec::Deflate),
    (".bz2", Codec::Bzip2),
    (".zst", Codec::Zstandard),
    (".snappy", Codec::Snappy),
    (".lz4", Codec::Lz4),
    (".lzo", Codec::Lzo),
    (".lzo_deflate", Codec::Lzo),
];

/// How many bytes the length written in front of a block or a chunk takes.
const LENGTH_BYTES: usize = 4;

/// Why one object's bytes are not the text its key says it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotDecompressed {
    /// The object's bytes are not the stream its key names, in the decoder's own words.
    NotTheCodec(String),
    /// What the object decompresses to is more than the read may hold.
    TooLarge,
    /// The codec its key names has no decoder in this emulator.
    NoDecoder(Codec),
}

/// The codec the object at `key` carries, or `None` for a key that names none.
///
/// The object's own NAME decides it, not the prefixes above it: the match is on what follows
/// the last separator, so a PREFIX whose name ends in an extension says nothing about the
/// objects under it. The extension matches as it is WRITTEN, which is how the codec factory
/// looks a codec up — an object keyed in another case is not a compressed split.
pub fn of_key(key: &str) -> Option<Codec> {
    let name = key.rsplit('/').next().unwrap_or(key);
    EXTENSIONS
        .iter()
        .find(|(extension, _)| name.len() > extension.len() && name.ends_with(extension))
        .map(|(_, codec)| *codec)
}

impl Codec {
    /// The extension a key carries this codec under.
    pub fn extension(self) -> &'static str {
        EXTENSIONS
            .iter()
            .find(|(_, codec)| *codec == self)
            .map_or("", |(extension, _)| *extension)
    }

    /// `body` as this codec's decoder reads it, holding at most `limit` bytes of output, or
    /// why the object is not the stream its key names.
    pub fn decompress(self, body: &[u8], limit: usize) -> Result<Vec<u8>, NotDecompressed> {
        // One byte past what the read may hold, so output that REACHES the bound is told
        // from output that exceeds it.
        let bound = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
        let mut decoded = Vec::new();
        match self {
            // A gzip or bzip2 stream may be the CONCATENATION of several members, which a
            // writer appending to an object leaves behind, and all of them are its data.
            Self::Gzip => {
                read_bounded(::flate2::read::MultiGzDecoder::new(body), bound, &mut decoded)?;
            }
            Self::Deflate => {
                read_bounded(::flate2::read::ZlibDecoder::new(body), bound, &mut decoded)?;
            }
            Self::Bzip2 => {
                read_bounded(::bzip2::read::MultiBzDecoder::new(body), bound, &mut decoded)?;
            }
            Self::Zstandard => {
                let decoder = ::ruzstd::decoding::StreamingDecoder::new(body)
                    .map_err(|reason| NotDecompressed::NotTheCodec(reason.to_string()))?;
                read_bounded(decoder, bound, &mut decoded)?;
            }
            Self::Snappy | Self::Lz4 => {
                read_framed_blocks(self, body, limit, &mut decoded)?;
            }
            Self::Lzo => return Err(NotDecompressed::NoDecoder(self)),
        }
        if decoded.len() > limit {
            return Err(NotDecompressed::TooLarge);
        }
        Ok(decoded)
    }
}

/// Read everything `reader` produces into `into`, stopping after `bound` bytes.
fn read_bounded(
    reader: impl Read,
    bound: u64,
    into: &mut Vec<u8>,
) -> Result<(), NotDecompressed> {
    reader
        .take(bound)
        .read_to_end(into)
        .map(|_| ())
        .map_err(|reason| NotDecompressed::NotTheCodec(reason.to_string()))
}

/// `body` as the BLOCK-framed codecs hold a compressed object, decoded into `into`.
///
/// Such a stream is a sequence of BLOCKS, each written as the number of bytes it holds once
/// decompressed followed by the CHUNKS that carry them, and each chunk as the number of
/// compressed bytes it holds followed by those bytes. Only those lengths are read here:
/// every chunk's bytes are handed to the codec's own decoder.
///
/// Every length a block or a chunk DECLARES comes out of the object, which a caller wrote,
/// so none of them is allocated against: a declaration larger than what is left of `limit`
/// stops the read before a byte is held for it.
fn read_framed_blocks(
    codec: Codec,
    body: &[u8],
    limit: usize,
    into: &mut Vec<u8>,
) -> Result<(), NotDecompressed> {
    let mut rest = body;
    while !rest.is_empty() {
        let mut holds = take_length(&mut rest)?;
        if holds > limit.saturating_sub(into.len()) {
            return Err(NotDecompressed::TooLarge);
        }
        while holds > 0 {
            let compressed = take_length(&mut rest)?;
            let chunk = rest.get(..compressed).ok_or_else(truncated)?;
            rest = rest.get(compressed..).unwrap_or(&[]);
            let decoded = decompress_chunk(codec, chunk, holds)?;
            if into.len().saturating_add(decoded.len()) > limit {
                return Err(NotDecompressed::TooLarge);
            }
            into.extend_from_slice(&decoded);
            // A chunk that holds nothing leaves the block it belongs to unfinished, so the
            // block ends with it rather than being read for ever.
            if decoded.is_empty() {
                break;
            }
            holds = holds.saturating_sub(decoded.len());
        }
    }
    Ok(())
}

/// The length written in front of a block or a chunk, taken off the front of `rest`.
fn take_length(rest: &mut &[u8]) -> Result<usize, NotDecompressed> {
    let written: [u8; LENGTH_BYTES] = rest
        .get(..LENGTH_BYTES)
        .and_then(|bytes| <[u8; LENGTH_BYTES]>::try_from(bytes).ok())
        .ok_or_else(truncated)?;
    *rest = rest.get(LENGTH_BYTES..).unwrap_or(&[]);
    Ok(usize::try_from(u32::from_be_bytes(written)).unwrap_or(usize::MAX))
}

/// One chunk of a block-framed stream as the codec's own decoder reads it. `holds` is what
/// is left of the block the chunk belongs to, which is already bounded by what the read may
/// hold and therefore bounds what decoding the chunk may allocate.
fn decompress_chunk(
    codec: Codec,
    chunk: &[u8],
    holds: usize,
) -> Result<Vec<u8>, NotDecompressed> {
    match codec {
        // A raw LZ4 block carries no length of its own, so what it decodes to is bounded by
        // what is left of its block.
        Codec::Lz4 => ::lz4_flex::block::decompress(chunk, holds)
            .map_err(|reason| NotDecompressed::NotTheCodec(reason.to_string())),
        // A raw Snappy chunk DECLARES the length it decodes to in its own header, which the
        // object carries and a caller therefore chose, so the declaration is read and
        // checked before the decoder is asked to hold that much.
        Codec::Snappy => {
            let declared = ::snap::raw::decompress_len(chunk)
                .map_err(|reason| NotDecompressed::NotTheCodec(reason.to_string()))?;
            if declared > holds {
                return Err(NotDecompressed::TooLarge);
            }
            ::snap::raw::Decoder::new()
                .decompress_vec(chunk)
                .map_err(|reason| NotDecompressed::NotTheCodec(reason.to_string()))
        }
        other => Err(NotDecompressed::NoDecoder(other)),
    }
}

/// A stream that ends in the middle of a block or a chunk is not the stream its key names.
fn truncated() -> NotDecompressed {
    NotDecompressed::NotTheCodec(
        "the compressed stream ends inside the block it was reading".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use ::std::io::Write;

    use super::*;

    /// The text every round trip here compresses: long enough that each codec has something
    /// to compress, and lines rather than one token, because what it stands for is one text
    /// object of a table.
    const RECORDS: &str = "1\tdesk\t249.5\n2\tlamp\t19.0\n3\tchair\t89.25\n4\tsofa\t999.0\n";

    /// One object compressed with `codec`, written by that codec's own encoder.
    ///
    /// The two block-framed codecs have no stream encoder here, so their framing is written
    /// out: the length the block decodes to, then one chunk — the length of its compressed
    /// bytes and the bytes the codec's own compressor produced.
    fn compressed(codec: Codec, body: &[u8]) -> Vec<u8> {
        match codec {
            Codec::Gzip => {
                let mut encoder = ::flate2::write::GzEncoder::new(
                    Vec::new(),
                    ::flate2::Compression::default(),
                );
                encoder.write_all(body).expect("the encoder takes the body");
                encoder.finish().expect("the gzip stream")
            }
            Codec::Deflate => {
                let mut encoder = ::flate2::write::ZlibEncoder::new(
                    Vec::new(),
                    ::flate2::Compression::default(),
                );
                encoder.write_all(body).expect("the encoder takes the body");
                encoder.finish().expect("the zlib stream")
            }
            Codec::Bzip2 => {
                let mut encoder =
                    ::bzip2::write::BzEncoder::new(Vec::new(), ::bzip2::Compression::default());
                encoder.write_all(body).expect("the encoder takes the body");
                encoder.finish().expect("the bzip2 stream")
            }
            Codec::Zstandard => ::ruzstd::encoding::compress_to_vec(
                body,
                ::ruzstd::encoding::CompressionLevel::Fastest,
            ),
            Codec::Snappy | Codec::Lz4 => {
                let chunk = match codec {
                    Codec::Lz4 => ::lz4_flex::block::compress(body),
                    _ => ::snap::raw::Encoder::new()
                        .compress_vec(body)
                        .expect("the Snappy chunk"),
                };
                let mut framed = Vec::new();
                framed.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
                framed.extend_from_slice(&u32::try_from(chunk.len()).unwrap().to_be_bytes());
                framed.extend_from_slice(&chunk);
                framed
            }
            Codec::Lzo => body.to_vec(),
        }
    }

    /// An object key names the codec its extension belongs to: the rows differ along the
    /// extension, the prefixes above the object, the case the extension is written in and a
    /// key that names no codec at all.
    #[test]
    fn an_object_key_names_the_codec_its_extension_belongs_to() {
        // (the key, the codec it names)
        let cases: Vec<(&str, Option<Codec>)> = vec![
            ("logs.gz", Some(Codec::Gzip)),
            ("E2ABC.2026-01-01-12.abc123.gz", Some(Codec::Gzip)),
            ("year=2026/month=01/part-0.gz", Some(Codec::Gzip)),
            ("rows.deflate", Some(Codec::Deflate)),
            ("rows.bz2", Some(Codec::Bzip2)),
            ("rows.zst", Some(Codec::Zstandard)),
            ("rows.snappy", Some(Codec::Snappy)),
            ("rows.lz4", Some(Codec::Lz4)),
            ("rows.lzo", Some(Codec::Lzo)),
            ("rows.lzo_deflate", Some(Codec::Lzo)),
            ("rows.tsv", None),
            ("rows.csv", None),
            ("rows", None),
            // The extension matches as the codec factory spells it, so another case of it
            // is part of the name rather than a codec.
            ("rows.GZ", None),
            // A PREFIX whose own name ends in an extension says nothing about what the
            // objects under it hold.
            ("archive.gz/rows.tsv", None),
        ];

        for (key, expected) in cases {
            let actual = of_key(key);
            assert_eq!(
                actual, expected,
                "case {key:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// Every codec a key can name reads back exactly the bytes that were compressed with
    /// it, so what the SerDe then reads is the object's own text.
    #[test]
    fn a_compressed_object_decompresses_to_the_text_it_holds() {
        let cases: Vec<Codec> = vec![
            Codec::Gzip,
            Codec::Deflate,
            Codec::Bzip2,
            Codec::Zstandard,
            Codec::Snappy,
            Codec::Lz4,
        ];

        for codec in cases {
            let body = compressed(codec, RECORDS.as_bytes());
            let actual = codec
                .decompress(&body, RECORDS.len())
                .unwrap_or_else(|reason| {
                    panic!("case {codec:?}: the object must decompress, got {reason:?}")
                });
            assert_eq!(
                String::from_utf8_lossy(&actual),
                RECORDS,
                "case {codec:?}: expected the text that was compressed, got {} bytes",
                actual.len()
            );
        }
    }

    /// An empty object is empty text under every codec that frames its blocks itself, which
    /// is what a location holding a placeholder object reads as.
    #[test]
    fn an_empty_block_framed_object_decompresses_to_nothing() {
        for codec in [Codec::Snappy, Codec::Lz4] {
            let actual = codec
                .decompress(&[], 1024)
                .unwrap_or_else(|reason| panic!("case {codec:?}: expected no rows, got {reason:?}"));
            assert!(
                actual.is_empty(),
                "case {codec:?}: expected nothing, got {} bytes",
                actual.len()
            );
        }
    }

    /// An object that decompresses to more than the read may hold is refused at the bound
    /// rather than held: the rows differ along the codec, and the bound is one byte under
    /// what the text holds.
    #[test]
    fn an_object_over_the_bound_is_refused() {
        for codec in [
            Codec::Gzip,
            Codec::Deflate,
            Codec::Bzip2,
            Codec::Zstandard,
            Codec::Snappy,
            Codec::Lz4,
        ] {
            let body = compressed(codec, RECORDS.as_bytes());
            let actual = codec.decompress(&body, RECORDS.len() - 1);
            assert_eq!(
                actual,
                Err(NotDecompressed::TooLarge),
                "case {codec:?}: expected the bound to refuse it, got {actual:?}"
            );
        }
    }

    /// An object whose key names a codec its bytes are not is reported as that codec's
    /// decoder refusing it, never read as the text it is not.
    ///
    /// The codecs whose stream carries its own header say so themselves. The block-framed
    /// ones read their first bytes as the length of a block, which for arbitrary text names
    /// a block far larger than any read holds, so they refuse it at that bound — either way
    /// the bytes never reach the SerDe.
    #[test]
    fn an_object_that_is_not_its_codecs_stream_is_reported() {
        for codec in [
            Codec::Gzip,
            Codec::Deflate,
            Codec::Bzip2,
            Codec::Zstandard,
        ] {
            let actual = codec.decompress(RECORDS.as_bytes(), 1024);
            assert!(
                matches!(actual, Err(NotDecompressed::NotTheCodec(_))),
                "case {codec:?}: expected the decoder to refuse plain text, got {actual:?}"
            );
        }
        for codec in [Codec::Snappy, Codec::Lz4] {
            let actual = codec.decompress(RECORDS.as_bytes(), 1024);
            assert!(
                actual.is_err(),
                "case {codec:?}: expected plain text to be refused, got {actual:?}"
            );
        }
    }

    /// A block-framed object that DECLARES more than the read may hold is refused before
    /// anything is held for it, whether the declaration is the BLOCK's length or the one a
    /// Snappy chunk carries in its own header: both come out of the object, so a tiny object
    /// must not be able to make a read allocate what it names.
    #[test]
    fn a_declaration_larger_than_the_bound_is_refused_before_anything_is_held() {
        // The length every case declares: more than any read here may hold.
        let declared = u32::MAX.to_be_bytes();
        let mut block = Vec::new();
        block.extend_from_slice(&declared);
        block.extend_from_slice(&4u32.to_be_bytes());
        block.extend_from_slice(b"rows");
        // A block of a size the bound admits, whose one chunk's own header declares far
        // more: the varint of that length, followed by the chunk's first tag.
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&8u32.to_be_bytes());
        chunk.extend_from_slice(&6u32.to_be_bytes());
        chunk.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 0x00]);
        // (case, the codec, the object)
        let cases: Vec<(&str, Codec, &Vec<u8>)> = vec![
            ("a Snappy block declaring more than the bound", Codec::Snappy, &block),
            ("an LZ4 block declaring more than the bound", Codec::Lz4, &block),
            ("a Snappy chunk declaring more than its block", Codec::Snappy, &chunk),
        ];

        for (case, codec, object) in cases {
            let actual = codec.decompress(object, 1024);
            assert_eq!(
                actual,
                Err(NotDecompressed::TooLarge),
                "case {case:?}: expected the declaration to be refused, got {actual:?}"
            );
        }
    }

    /// The one codec with no decoder here is reported as such — the extension is recognised,
    /// so the object is never handed to the SerDe as the compressed bytes it stores.
    #[test]
    fn a_codec_with_no_decoder_is_reported() {
        let actual = Codec::Lzo.decompress(RECORDS.as_bytes(), 1024);
        assert_eq!(
            actual,
            Err(NotDecompressed::NoDecoder(Codec::Lzo)),
            "expected the codec to be reported as undecodable, got {actual:?}"
        );
        assert_eq!(
            Codec::Lzo.extension(),
            ".lzo",
            "expected the extension the codec is keyed under, got {:?}",
            Codec::Lzo.extension()
        );
    }
}
