// SPDX-License-Identifier: CC0-1.0

//! The DICOM metadata of one SOP INSTANCE an image set holds, and what is computed from
//! the pixel data of one of its frames.
//!
//! An image set is a series of a study, and the attributes of the patient, the study and
//! that series are the modelled ones ([`super::DICOMTags`]). Everything BELOW the series
//! belongs to one SOP instance: its own DICOM header — the file meta group a Part 10 file
//! opens with, the SOP identifiers, the image pixel description — and the image frames its
//! pixel data was stored as. Those are what the aggregated metadata document reports under
//! `Study` → `Series` → `Instances`, and what this module holds.
//!
//! Each attribute is kept under its DICOM KEYWORD, typed as its value representation
//! writes it — a text VR as text, an integer VR as a number, a binary VR as base64 — and
//! the VR itself is kept beside it, so a reader of the document can tell the two apart
//! without a dictionary of its own.
//!
//! What is reported ABOUT a frame is computed from the pixel data that frame really holds
//! ([`PixelDescription::frame_metadata`]): how many bytes it is, the smallest and largest
//! sample in it, and the checksum of it. Nothing here is computed for pixel data that is
//! not the raster the instance's own image pixel description declares, because the two
//! would then describe different content.

use ::std::collections::BTreeMap;

use ::serde_json::Value;

/// The checksum algorithm [`pixel_data_checksum`] computes a frame's
/// [`PixelDataChecksum::checksum`] with — a 32-bit cyclic redundancy check.
///
/// Named HERE, beside the computation, so the one place the algorithm is chosen is the one
/// place it is named: the aggregated metadata document reports this as the checksum type of
/// an instance's stored pixel data, and so can never name a type the checksums it carries
/// were not summed with.
pub const PIXEL_DATA_CHECKSUM_TYPE: &str = "CRC32";

/// The DICOM transfer syntax UID this service reports the pixel data it STORES as being in.
///
/// `GetImageFrame` documents the content type it answers for pixel data whose stored
/// transfer syntax is missing, and gives UID `1.2.840.10008.1.2.4.202` — High-Throughput
/// JPEG 2000 — the same answer ([`super::MISSING_TRANSFER_SYNTAX_CONTENT_TYPE`]), so that
/// is the one UID this service's frame reader already serves a stored frame's content type
/// by. The aggregated metadata document reports the stored transfer syntax as the same UID,
/// so the document and the frame read agree on the form the pixel data an image set holds
/// is served in.
pub const STORED_TRANSFER_SYNTAX_UID: &str = "1.2.840.10008.1.2.4.202";

/// The DICOM metadata one SOP instance of an image set holds.
///
/// Held per instance inside the image set's ATTRIBUTES ([`super::ImageSetAttributes`]), so
/// a version of an image set reports the instances that version held, a copy carries them
/// to its destination, and a deletion gives them up with the rest of the content.
#[derive(Debug, Clone, Default)]
pub struct ImageSetInstance {
    /// The instance's own DICOM attributes, keyed by DICOM keyword and typed as the
    /// attribute's value representation writes it.
    pub dicom: BTreeMap<String, Value>,
    /// The value representation of each attribute in [`ImageSetInstance::dicom`], keyed
    /// the same way.
    pub dicom_vrs: BTreeMap<String, String>,
    /// The DICOM transfer syntax UID the instance's data set — and so the pixel data taken
    /// out of it — is STORED in, `None` when nothing established one.
    pub stored_transfer_syntax_uid: Option<String>,
    /// What is reported about each image frame of this instance, keyed by the
    /// `ImageFrameId` the frame is stored under, so a frame and the record describing it
    /// cannot disagree.
    pub frames: BTreeMap<String, ImageFrameMetadata>,
}

/// What the aggregated metadata document reports about ONE image frame besides its
/// identifier: how large the stored pixel data is, the range of the samples in it, and the
/// checksum of it.
#[derive(Debug, Clone, Default)]
pub struct ImageFrameMetadata {
    /// How many bytes of pixel data the frame holds.
    pub frame_size_in_bytes: u64,
    /// The smallest and the largest sample the pixel data carries, `None` when the stored
    /// bytes are not the raster the instance declares and so carry no samples to measure.
    pub min_pixel_value: Option<i64>,
    pub max_pixel_value: Option<i64>,
    /// The checksum of the pixel data at each resolution it is held at, lowest resolution
    /// first. This service stores one resolution — the one the instance declares — so the
    /// base and the full resolution are the same entry.
    pub checksums: Vec<PixelDataChecksum>,
}

/// The checksum of one resolution of a frame's pixel data, with the raster that
/// resolution covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelDataChecksum {
    pub width: u32,
    pub height: u32,
    /// The [`PIXEL_DATA_CHECKSUM_TYPE`] checksum of the stored bytes.
    pub checksum: u32,
}

/// The image pixel description one DICOM instance declares — how the samples of its pixel
/// data are laid out.
///
/// Read off the instance when it is imported and used only to compute what is reported
/// about its frames: it is not persisted on its own, because every member of it is already
/// an attribute of the instance ([`ImageSetInstance::dicom`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct PixelDescription {
    pub rows: Option<u32>,
    pub columns: Option<u32>,
    pub samples_per_pixel: Option<u32>,
    pub bits_allocated: Option<u32>,
    /// `0` for unsigned samples and `1` for two's-complement signed ones, as DICOM's
    /// Pixel Representation attribute spells it.
    pub pixel_representation: Option<u32>,
}

impl PixelDescription {
    /// What is reported about a frame holding `pixel_data` under this description.
    ///
    /// The size and the checksum are of the bytes themselves, so they are reported for
    /// every frame. The sample range is measured only when the bytes ARE the raster this
    /// description declares — the rows, the columns, the samples per pixel and the bits
    /// per sample all account for exactly the bytes held — because a range measured over
    /// anything else would describe pixel data the instance does not declare.
    pub fn frame_metadata(&self, pixel_data: &[u8]) -> ImageFrameMetadata {
        let (min_pixel_value, max_pixel_value) = match self.sample_range(pixel_data) {
            Some((min, max)) => (Some(min), Some(max)),
            None => (None, None),
        };
        ImageFrameMetadata {
            frame_size_in_bytes: pixel_data.len() as u64,
            min_pixel_value,
            max_pixel_value,
            checksums: match (self.columns, self.rows) {
                (Some(width), Some(height)) => vec![PixelDataChecksum {
                    width,
                    height,
                    checksum: pixel_data_checksum(pixel_data),
                }],
                // A frame whose raster nothing declared is a frame no resolution can be
                // stated for, so no checksum entry is reported for it.
                _ => Vec::new(),
            },
        }
    }

    /// The smallest and largest sample `pixel_data` carries under this description, or
    /// `None` when the bytes are not the raster it declares.
    fn sample_range(&self, pixel_data: &[u8]) -> Option<(i64, i64)> {
        let rows = usize::try_from(self.rows?).ok()?;
        let columns = usize::try_from(self.columns?).ok()?;
        let samples_per_pixel = usize::try_from(self.samples_per_pixel.unwrap_or(1)).ok()?;
        let bits_allocated = self.bits_allocated?;
        let bytes_per_sample = match bits_allocated {
            8 => 1_usize,
            16 => 2,
            // A sample width this service does not lay out: nothing is measured rather
            // than measured wrongly.
            _ => return None,
        };
        let declared = rows
            .checked_mul(columns)?
            .checked_mul(samples_per_pixel)?
            .checked_mul(bytes_per_sample)?;
        if declared == 0 || declared != pixel_data.len() {
            return None;
        }
        let signed = self.pixel_representation == Some(1);
        let mut min = i64::MAX;
        let mut max = i64::MIN;
        for sample in pixel_data.chunks_exact(bytes_per_sample) {
            // Matched on the chunk itself, so nothing indexes past what the chunk holds.
            let value = match (sample, signed) {
                ([low], false) => i64::from(*low),
                ([low], true) => i64::from(i8::from_le_bytes([*low])),
                // DICOM writes a 16-bit sample little-endian in every transfer syntax
                // this service stores pixel data from.
                ([low, high], false) => i64::from(u16::from_le_bytes([*low, *high])),
                ([low, high], true) => i64::from(i16::from_le_bytes([*low, *high])),
                _ => return None,
            };
            min = min.min(value);
            max = max.max(value);
        }
        Some((min, max))
    }
}

/// The [`PIXEL_DATA_CHECKSUM_TYPE`] checksum of `pixel_data`, from a maintained codec
/// rather than a table written here.
pub fn pixel_data_checksum(pixel_data: &[u8]) -> u32 {
    let mut hasher = ::crc32fast::Hasher::new();
    hasher.update(pixel_data);
    hasher.finalize()
}

/// Deliver `delivered` into the instances `held`, INSTANCE BY INSTANCE: an instance the
/// delivery carries takes the place of the one held under the same SOP Instance UID, and
/// an instance the delivery says nothing about stays where it was.
///
/// The rule every other half of an image set's attributes is delivered by
/// ([`super::ImageSetAttributes::merge`]): a delivery describes the content it carries, so
/// it replaces what it describes and leaves the rest.
pub fn merge_instances(
    held: &mut BTreeMap<String, ImageSetInstance>,
    delivered: BTreeMap<String, ImageSetInstance>,
) {
    for (sop_instance_uid, instance) in delivered {
        held.insert(sop_instance_uid, instance);
    }
}
