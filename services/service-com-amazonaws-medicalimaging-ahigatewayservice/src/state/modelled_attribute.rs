// SPDX-License-Identifier: CC0-1.0

//! What a modelled `DICOMTags` member may HOLD, and the requirement a value that it cannot
//! hold fails.
//!
//! The model declares sixteen DICOM attributes on `DICOMTags`
//! (`smithy_query(query="shape",shape="DICOMTags")`), each a constrained shape: a kind, a
//! `@length`, a `@pattern`, a `@range`. Those members are what `SearchImageSets` reports in
//! `ImageSetsMetadataSummary$DICOMTags` and what the aggregated metadata document
//! `GetImageSetMetadata` serves reports at the patient, study and series levels, so a value
//! stored in one of them is a value the model promises the member can carry.
//!
//! Nothing the BOUNDARY validator walks reaches these members: an attribute arrives either
//! inside a blob (`UpdateImageSetMetadata`'s update document) or out of a DICOM Part 10
//! file an import read ([`super::read_part10`]) — neither of which the validator looks
//! inside. So every constraint the model declares on the member an attribute is destined
//! for is applied HERE, once, for every writer of that member: a value that fails one is
//! not stored, and the writer states the requirement it failed
//! ([`ModelledAttribute::unmet_requirement`]) in the terms the boundary would have stated
//! it in. Without that, a search summary and a served metadata document would report values
//! the model forbids.

use ::serde_json::Value;
use ::smithy_server_runtime::ViolationKind;

use super::DICOMTags;

/// What one modelled attribute holds in the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeKind {
    Text,
    Integer,
}

impl AttributeKind {
    /// How a refusal names this kind.
    fn described(self) -> &'static str {
        match self {
            Self::Text => "a string",
            Self::Integer => "an integer",
        }
    }
}

/// The `@range` MAXIMUM the model declares on both study counts —
/// `DICOMNumberOfStudyRelatedSeries` and `DICOMNumberOfStudyRelatedInstances`
/// (`smithy_query(query="shape",shape="DICOMTags")`), each `@range(min: 0, max: 1000000)`.
pub const MAX_STUDY_RELATED_COUNT: i64 = 1_000_000;

/// The `@range` `DICOMSeriesNumber` declares
/// (`smithy_query(query="shape",shape="DICOMTags")`:
/// `@range(min: -2147483648, max: 2147483647)`) — the whole of the 32-bit integer the
/// model types every numeric member of the shape at, so it bounds any of them that
/// declares no narrower range of its own.
pub const INTEGER_MEMBER_RANGE: (i64, i64) = (i32::MIN as i64, i32::MAX as i64);

/// The `@pattern` a modelled attribute's member declares. The model declares ONE
/// expression across the members that have one, so it is named rather than compiled: a
/// variant per expression, with the text to quote in a refusal and the test to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributePattern {
    /// What a DICOM Unique Identifier is: the pattern both the Study and the Series
    /// Instance UID members declare.
    DicomUid,
}

impl AttributePattern {
    /// The expression itself, copied verbatim from the model
    /// (`smithy_query(query="shape",shape="DICOMTags")`) — the text a refusal quotes.
    fn declared(self) -> &'static str {
        match self {
            Self::DicomUid => "^[0-9.]+$",
        }
    }

    /// Whether `text` satisfies it.
    ///
    /// A character class is a MEMBERSHIP test rather than a pattern language, so the class
    /// and the `+` quantifier between the `^` and the `$` are applied character by
    /// character — the way this service applies the pattern on a tag key
    /// ([`super::tag_key_matches_pattern`]): the check then costs one pass over the
    /// value with no regex engine on a path that reads request text.
    fn matches(self, text: &str) -> bool {
        match self {
            Self::DicomUid => {
                !text.is_empty() && text.chars().all(|digit| digit.is_ascii_digit() || digit == '.')
            }
        }
    }
}

/// The LEVEL of the DICOM hierarchy one modelled attribute describes — the level of the
/// aggregated metadata document that reports it, which is where its own name puts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeLevel {
    Patient,
    Study,
    Series,
}

/// One attribute the MODEL names: the `DICOMTags` member the document's keyword at that
/// level maps to.
///
/// The members are the model's own
/// (`smithy_query(query="shape",shape="DICOMTags")`), each under the DICOM keyword the
/// aggregated metadata document spells it with — the keyword
/// `crate::ops::get_image_set_metadata` reports it under, so one attribute is written and
/// read under one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelledAttribute {
    PatientId,
    PatientName,
    PatientBirthDate,
    PatientSex,
    StudyInstanceUid,
    StudyId,
    StudyDescription,
    StudyDate,
    StudyTime,
    AccessionNumber,
    StudyRelatedSeries,
    StudyRelatedInstances,
    SeriesInstanceUid,
    SeriesModality,
    SeriesBodyPart,
    SeriesNumber,
}

/// Every attribute the model names, in the order the shape declares the members — the whole
/// of what a writer of `DICOMTags` may write, so a rule stated over this list reaches each
/// of them.
pub const MODELLED_ATTRIBUTES: [ModelledAttribute; 16] = [
    ModelledAttribute::PatientId,
    ModelledAttribute::PatientName,
    ModelledAttribute::PatientBirthDate,
    ModelledAttribute::PatientSex,
    ModelledAttribute::StudyInstanceUid,
    ModelledAttribute::StudyId,
    ModelledAttribute::StudyDescription,
    ModelledAttribute::StudyDate,
    ModelledAttribute::StudyTime,
    ModelledAttribute::AccessionNumber,
    ModelledAttribute::StudyRelatedSeries,
    ModelledAttribute::StudyRelatedInstances,
    ModelledAttribute::SeriesInstanceUid,
    ModelledAttribute::SeriesModality,
    ModelledAttribute::SeriesBodyPart,
    ModelledAttribute::SeriesNumber,
];

impl ModelledAttribute {
    /// What this attribute holds in the document: the two study counts and the series
    /// number are numbers, every other modelled attribute is text.
    pub fn kind(self) -> AttributeKind {
        match self {
            Self::StudyRelatedSeries | Self::StudyRelatedInstances | Self::SeriesNumber => {
                AttributeKind::Integer
            }
            _ => AttributeKind::Text,
        }
    }

    /// The DICOM KEYWORD this attribute is spelled with — the name the aggregated metadata
    /// document carries it under, and the name a refusal about it states.
    pub fn keyword(self) -> &'static str {
        match self {
            Self::PatientId => "PatientID",
            Self::PatientName => "PatientName",
            Self::PatientBirthDate => "PatientBirthDate",
            Self::PatientSex => "PatientSex",
            Self::StudyInstanceUid => "StudyInstanceUID",
            Self::StudyId => "StudyID",
            Self::StudyDescription => "StudyDescription",
            Self::StudyDate => "StudyDate",
            Self::StudyTime => "StudyTime",
            Self::AccessionNumber => "AccessionNumber",
            Self::StudyRelatedSeries => "NumberOfStudyRelatedSeries",
            Self::StudyRelatedInstances => "NumberOfStudyRelatedInstances",
            Self::SeriesInstanceUid => "SeriesInstanceUID",
            Self::SeriesModality => "Modality",
            Self::SeriesBodyPart => "BodyPartExamined",
            Self::SeriesNumber => "SeriesNumber",
        }
    }

    /// The level of the DICOM hierarchy this attribute describes, which its own member name
    /// gives it.
    pub fn level(self) -> AttributeLevel {
        match self {
            Self::PatientId | Self::PatientName | Self::PatientBirthDate | Self::PatientSex => {
                AttributeLevel::Patient
            }
            Self::StudyInstanceUid
            | Self::StudyId
            | Self::StudyDescription
            | Self::StudyDate
            | Self::StudyTime
            | Self::AccessionNumber
            | Self::StudyRelatedSeries
            | Self::StudyRelatedInstances => AttributeLevel::Study,
            Self::SeriesInstanceUid
            | Self::SeriesModality
            | Self::SeriesBodyPart
            | Self::SeriesNumber => AttributeLevel::Series,
        }
    }

    /// The attribute the model names at `level` under `keyword`, or `None` when it names
    /// none there — the one lookup from a document's own spelling to the member that holds
    /// it, so a keyword is mapped by the same name it is reported under.
    pub fn at(level: AttributeLevel, keyword: &str) -> Option<Self> {
        MODELLED_ATTRIBUTES
            .into_iter()
            .find(|attribute| attribute.level() == level && attribute.keyword() == keyword)
    }

    /// Whether `value` is one this attribute's modelled member can legally carry: of the
    /// kind the member holds AND within every constraint the model declares on it
    /// ([`ModelledAttribute::unmet_requirement`]).
    pub fn admits(self, value: &Value) -> bool {
        self.unmet_requirement(value).is_none()
    }

    /// The `@length` MAXIMUM the model declares on this attribute's member, or `None` for
    /// one holding a number (`smithy_query(query="shape",shape="DICOMTags")`).
    ///
    /// Each text member's `@length` minimum is 0, which every string satisfies, so the
    /// maximum is the whole of the bound there is to apply.
    pub fn max_length(self) -> Option<usize> {
        match self {
            Self::PatientId
            | Self::PatientName
            | Self::StudyInstanceUid
            | Self::StudyId
            | Self::StudyDescription
            | Self::AccessionNumber
            | Self::SeriesInstanceUid => Some(512),
            Self::PatientBirthDate | Self::StudyDate => Some(36),
            Self::StudyTime => Some(56),
            Self::PatientSex => Some(16),
            Self::SeriesModality => Some(32),
            Self::SeriesBodyPart => Some(128),
            Self::StudyRelatedSeries | Self::StudyRelatedInstances | Self::SeriesNumber => None,
        }
    }

    /// The `@pattern` the model declares on this attribute's member, or `None` when it
    /// declares none (`smithy_query(query="shape",shape="DICOMTags")`): the two UID members
    /// are the ones that carry one.
    fn pattern(self) -> Option<AttributePattern> {
        match self {
            Self::StudyInstanceUid | Self::SeriesInstanceUid => Some(AttributePattern::DicomUid),
            _ => None,
        }
    }

    /// The `@range` this attribute's member carries, as the inclusive pair of bounds
    /// (`smithy_query(query="shape",shape="DICOMTags")`): the two study counts are bounded
    /// at a million, and every other numeric member carries
    /// [`INTEGER_MEMBER_RANGE`] — the `@range` the series number declares, which is also
    /// the integer width the model types the shape's numeric members at, so a numeric
    /// member declaring no `@range` of its own is still bounded by what it can hold.
    ///
    /// Read only for a member [`ModelledAttribute::kind`] says holds a number.
    pub fn range(self) -> (i64, i64) {
        match self {
            Self::StudyRelatedSeries | Self::StudyRelatedInstances => (0, MAX_STUDY_RELATED_COUNT),
            _ => INTEGER_MEMBER_RANGE,
        }
    }

    /// The requirement `value` FAILS as this attribute's modelled member, or `None` when
    /// it satisfies every one of them.
    ///
    /// The member is a constrained shape, so a document may only write a value the member
    /// can carry: of the kind it holds, then every constraint the model declares on it.
    pub fn unmet_requirement(self, value: &Value) -> Option<String> {
        match self.kind() {
            AttributeKind::Integer => self.unmet_number_requirement(value),
            AttributeKind::Text => self.unmet_text_requirement(value),
        }
    }

    /// [`ModelledAttribute::unmet_requirement`] for a member holding a NUMBER: a JSON
    /// integer, inside the member's `@range`.
    ///
    /// The requirement is worded by the framework
    /// ([`super::constraint_requirement`]), so a refusal stated here reads like the
    /// one the boundary states for the same constraint on a member it does walk.
    fn unmet_number_requirement(self, value: &Value) -> Option<String> {
        let Some(number) = value.as_i64() else {
            return Some(self.kind_requirement());
        };
        let (min, max) = self.range();
        let violated = if number < min {
            Some(ViolationKind::MinValue(min.to_string()))
        } else if number > max {
            Some(ViolationKind::MaxValue(max.to_string()))
        } else {
            None
        };
        violated.map(super::constraint_requirement)
    }

    /// [`ModelledAttribute::unmet_requirement`] for a member holding TEXT: a JSON string,
    /// inside the member's `@length` and matching its `@pattern`.
    ///
    /// The two constraints are checked in the order the boundary validator's own leaf walk
    /// runs the traits — the length bound, then the pattern — so a value breaking both is
    /// refused the way the boundary would have refused it.
    fn unmet_text_requirement(self, value: &Value) -> Option<String> {
        let Some(text) = value.as_str() else {
            return Some(self.kind_requirement());
        };
        // `@length` on a string counts CHARACTERS, which is how the boundary validator
        // counts the same member's length.
        let length = text.chars().count();
        let over_long = self.max_length().filter(|max| length > *max);
        let violated = match (over_long, self.pattern()) {
            (Some(max), _) => Some(ViolationKind::MaxLength(max as i64)),
            (None, Some(pattern)) if !pattern.matches(text) => {
                Some(ViolationKind::Pattern(pattern.declared().to_string()))
            }
            _ => None,
        };
        violated.map(super::constraint_requirement)
    }

    /// The requirement a value of the WRONG KIND for this attribute fails — the one
    /// requirement no modelled constraint states, since the document carries a value's kind
    /// where a member's schema states it.
    fn kind_requirement(self) -> String {
        format!("Member must hold {}", self.kind().described())
    }

    /// What this attribute's modelled member HOLDS, spelled as the aggregated metadata
    /// document spells the value, or `None` when nothing is stored there — the read that
    /// pairs with [`ModelledAttribute::write`].
    ///
    /// The model types the two study counts as plain numbers rather than optional ones, so
    /// each of them always holds one: zero is what an image set nothing has stored a count
    /// for holds, which is what the document reports for it.
    pub fn stored(self, tags: &DICOMTags) -> Option<Value> {
        let text = |held: &Option<String>| held.clone().map(Value::String);
        match self {
            Self::PatientId => text(&tags.dicom_patient_id),
            Self::PatientName => text(&tags.dicom_patient_name),
            Self::PatientBirthDate => text(&tags.dicom_patient_birth_date),
            Self::PatientSex => text(&tags.dicom_patient_sex),
            Self::StudyInstanceUid => text(&tags.dicom_study_instance_uid),
            Self::StudyId => text(&tags.dicom_study_id),
            Self::StudyDescription => text(&tags.dicom_study_description),
            Self::StudyDate => text(&tags.dicom_study_date),
            Self::StudyTime => text(&tags.dicom_study_time),
            Self::AccessionNumber => text(&tags.dicom_accession_number),
            Self::StudyRelatedSeries => {
                Some(Value::from(tags.dicom_number_of_study_related_series))
            }
            Self::StudyRelatedInstances => {
                Some(Value::from(tags.dicom_number_of_study_related_instances))
            }
            Self::SeriesInstanceUid => text(&tags.dicom_series_instance_uid),
            Self::SeriesModality => text(&tags.dicom_series_modality),
            Self::SeriesBodyPart => text(&tags.dicom_series_body_part),
            Self::SeriesNumber => tags.dicom_series_number.map(Value::from),
        }
    }

    /// Write `value` into this attribute's modelled member, or REMOVE what it held when
    /// `value` is `None`.
    ///
    /// A removed count reads back as zero rather than as absent: the model types the two
    /// study counts as plain numbers, so zero is what an image set nothing has stored a
    /// count for reports.
    pub fn write(self, tags: &mut DICOMTags, value: Option<&Value>) {
        let text = || value.and_then(Value::as_str).map(str::to_string);
        let number = || {
            value
                .and_then(Value::as_i64)
                .and_then(|number| i32::try_from(number).ok())
        };
        match self {
            Self::PatientId => tags.dicom_patient_id = text(),
            Self::PatientName => tags.dicom_patient_name = text(),
            Self::PatientBirthDate => tags.dicom_patient_birth_date = text(),
            Self::PatientSex => tags.dicom_patient_sex = text(),
            Self::StudyInstanceUid => tags.dicom_study_instance_uid = text(),
            Self::StudyId => tags.dicom_study_id = text(),
            Self::StudyDescription => tags.dicom_study_description = text(),
            Self::StudyDate => tags.dicom_study_date = text(),
            Self::StudyTime => tags.dicom_study_time = text(),
            Self::AccessionNumber => tags.dicom_accession_number = text(),
            Self::StudyRelatedSeries => {
                tags.dicom_number_of_study_related_series = number().unwrap_or_default();
            }
            Self::StudyRelatedInstances => {
                tags.dicom_number_of_study_related_instances = number().unwrap_or_default();
            }
            Self::SeriesInstanceUid => tags.dicom_series_instance_uid = text(),
            Self::SeriesModality => tags.dicom_series_modality = text(),
            Self::SeriesBodyPart => tags.dicom_series_body_part = text(),
            Self::SeriesNumber => tags.dicom_series_number = number(),
        }
    }
}

/// The modelled attribute `tags` holds a value its member CANNOT hold, together with the
/// requirement that value fails — `None` when every value held is one the model admits.
///
/// The check a writer that assembles a whole `DICOMTags` at once runs, where a writer of
/// one attribute at a time runs [`ModelledAttribute::unmet_requirement`] on the value it is
/// about to write: the attributes are walked in the order the shape declares them, so one
/// set of tags always fails on the same attribute.
pub fn unheld_requirement(tags: &DICOMTags) -> Option<(ModelledAttribute, String)> {
    MODELLED_ATTRIBUTES.into_iter().find_map(|attribute| {
        let held = attribute.stored(tags)?;
        attribute
            .unmet_requirement(&held)
            .map(|requirement| (attribute, requirement))
    })
}
