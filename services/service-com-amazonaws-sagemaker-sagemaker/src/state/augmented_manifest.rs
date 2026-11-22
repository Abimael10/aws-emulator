// SPDX-License-Identifier: CC0-1.0

//! The AUGMENTED MANIFEST a training channel can carry its OWN data in, read into the records
//! a run fits and measures on.
//!
//! # What the data type says
//!
//! "If you choose `AugmentedManifestFile`, `S3Uri` identifies an object that is an augmented
//! manifest file in JSON lines format. This file contains the data you want to use for model
//! training. `AugmentedManifestFile` can only be used if the Channel's input mode is `Pipe`"
//! (`S3DataSource$S3DataType`), and the attributes of each of those JSON lines a job takes are
//! "a list of one or more attribute names to use that are found in a specified augmented
//! manifest file" (`S3DataSource$AttributeNames`, `@length 0..16`).
//!
//! So this data type is unlike the other two: the location names ONE object, that object is not
//! a list of other objects ([`super::MANIFEST_FILE`]) nor a key name prefix (`S3Prefix`), and
//! the records are inside it — one JSON object per line, out of which the channel takes the
//! named attributes. A run reading such a channel as comma-separated rows
//! ([`super::read_records`]) finds no record in it at all, which is why the reading lives here.
//!
//! # The record one line holds
//!
//! The attributes are taken IN ORDER, which is what the data type's own layout means: the data
//! comes first and the label the training record is fitted against comes last. So the record of
//! one line is the values of the attributes the channel named — the LAST of them the record's
//! target, the ones before it its features — and a channel that named none takes every
//! attribute the line carries, which is the whole record the manifest holds.
//!
//! Each value is read as the numbers it carries: a number (or a string written as one) its own
//! value, a list the values inside it, and TEXT the frequency its words fall into
//! [`TEXT_FEATURE_WIDTH`] buckets — the hashing the text-classification learning task is fitted
//! over, so a manifest of sentences fits a model over the words those sentences hold rather
//! than over nothing. A label that is text rather than a number is the CLASS it names, numbered
//! in the order the classes appear in the manifest.
//!
//! A line that is not a JSON object, that does not carry every attribute the channel named, or
//! whose values hold no feature at all is COUNTED as skipped rather than guessed at, exactly as
//! a comma-separated row of another form is.
//!
//! Nothing here unwraps a record framing: `Channel$RecordWrapperType` describes how SageMaker
//! hands a record to the algorithm's container ("in `Pipe` mode... `RecordIO`"), not how the
//! manifest OBJECT is stored — the object in Amazon S3 is the JSON lines the caller wrote, and
//! that is what is read.

use super::*;

/// The `S3DataType` whose `S3Uri` "identifies an object that is an augmented manifest file in
/// JSON lines format. This file contains the data you want to use for model training"
/// (`S3DataSource$S3DataType`).
pub const AUGMENTED_MANIFEST_FILE: &str = "AugmentedManifestFile";

/// How many feature columns one TEXT attribute contributes: the buckets its words are counted
/// into.
///
/// An EMULATOR width, not an AWS limit: it is the dimension the text of a record is fitted in,
/// kept inside [`super::MAX_FEATURES`] so a record of a text attribute beside other attributes
/// is still a record the fit takes whole.
pub const TEXT_FEATURE_WIDTH: usize = 16;

/// Read the records one augmented manifest holds into `read`: one JSON line per record, out of
/// which the attributes `attribute_names` names are taken in order (every attribute the line
/// carries, when it names none).
///
/// A line that holds no record of that form is counted in `read.skipped`, which is what the
/// artifact then reports, and the read is bounded the same way the comma-separated one is: at
/// most [`super::MAX_RECORDS`] records, each of at most [`super::MAX_FEATURES`] features, all of
/// the width the first record carries.
pub fn read_augmented_records(bytes: &[u8], attribute_names: &[String], read: &mut ChannelData) {
    // The classes this manifest's labels name, in the order they appear: a label that is text
    // rather than a number is the class it names, and a class is numbered once.
    let mut classes: Vec<String> = Vec::new();
    let mut columns = read.records.first().map(|record| record.features.len());
    for line in bytes.split(|byte| *byte == b'\n') {
        if read.records.len() >= MAX_RECORDS {
            return;
        }
        let line = ::std::str::from_utf8(line).unwrap_or_default().trim();
        // The blank lines a JSON Lines document is laid out with hold no record and are not a
        // record of another form either.
        if line.is_empty() {
            continue;
        }
        let Ok(::serde_json::Value::Object(attributes)) = ::serde_json::from_str(line) else {
            read.skipped += 1;
            continue;
        };
        let taken: Option<Vec<&::serde_json::Value>> = if attribute_names.is_empty() {
            Some(attributes.values().collect())
        } else {
            attribute_names
                .iter()
                .map(|name| attributes.get(name))
                .collect()
        };
        // A line that does not carry every attribute the channel named is not a record of what
        // the channel asked for.
        let Some(taken) = taken else {
            read.skipped += 1;
            continue;
        };
        let Some((label, data)) = taken.split_last() else {
            read.skipped += 1;
            continue;
        };
        let Some(target) = class_of(label, &mut classes) else {
            read.skipped += 1;
            continue;
        };
        let features: Vec<f64> = data
            .iter()
            .flat_map(|value| attribute_features(value))
            .take(MAX_FEATURES)
            .collect();
        // A line whose data attributes carry no number at all holds no record to fit, and
        // neither does one of a width other than the first record's.
        if features.is_empty() || columns.is_some_and(|held| held != features.len()) {
            read.skipped += 1;
            continue;
        }
        columns = Some(features.len());
        read.records.push(Record { target, features });
    }
}

/// The numbers one attribute's value carries, as the record's features.
///
/// A number is its own value, a string written as a number that number, TEXT the frequency its
/// words fall into [`TEXT_FEATURE_WIDTH`] buckets, a boolean the number it reads as, and a list
/// or a nested object the values inside it — so an attribute carrying a vector contributes that
/// vector and one carrying a sentence contributes the sentence's own words.
///
/// The walk into a list or a nested object is as deep as the VALUE is, which the maintained JSON
/// parser that built it has already bounded: a line whose nesting passes that parser's own limit
/// is never parsed into a value at all, so no caller's line decides how deep this goes.
fn attribute_features(value: &::serde_json::Value) -> Vec<f64> {
    match value {
        ::serde_json::Value::Number(number) => number.as_f64().into_iter().collect(),
        ::serde_json::Value::Bool(flag) => vec![f64::from(u8::from(*flag))],
        ::serde_json::Value::String(text) => match text.trim().parse::<f64>() {
            Ok(number) => vec![number],
            Err(_) => text_features(text),
        },
        ::serde_json::Value::Array(items) => items.iter().flat_map(attribute_features).collect(),
        ::serde_json::Value::Object(members) => {
            members.values().flat_map(attribute_features).collect()
        }
        ::serde_json::Value::Null => Vec::new(),
    }
}

/// The class the label attribute `value` names, as the number the record is fitted against.
///
/// A label that is a number (or a string written as one) is that number. A label that is text is
/// the CLASS it names, numbered in the order `classes` first saw it, which is how a manifest of
/// named classes becomes records a model can be fitted on. A label nested inside an object or a
/// list is the first class the values inside it name, which is the form an augmented manifest
/// carries label metadata in.
fn class_of(value: &::serde_json::Value, classes: &mut Vec<String>) -> Option<f64> {
    match value {
        ::serde_json::Value::Number(number) => number.as_f64(),
        ::serde_json::Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        ::serde_json::Value::String(text) => match text.trim().parse::<f64>() {
            Ok(number) => Some(number),
            Err(_) => Some(class_index(text, classes)),
        },
        ::serde_json::Value::Array(items) => items
            .iter()
            .find_map(|item| class_of(item, classes)),
        ::serde_json::Value::Object(members) => members
            .values()
            .find_map(|member| class_of(member, classes)),
        ::serde_json::Value::Null => None,
    }
}

/// The number the class named `label` is held under: its position among the classes seen so
/// far, and the next position when it is new.
fn class_index(label: &str, classes: &mut Vec<String>) -> f64 {
    let named = label.trim();
    if let Some(at) = classes.iter().position(|class| class == named) {
        return at as f64;
    }
    classes.push(named.to_string());
    (classes.len() - 1) as f64
}

/// The features one TEXT attribute contributes: how often its words fall into each of
/// [`TEXT_FEATURE_WIDTH`] buckets, as a share of the words the text holds.
///
/// The bucket a word falls into is decided by the digest of the word itself (`sha2`, the one
/// digest this crate hashes with), so the same word is always the same feature column and a text
/// of any length is a record of the same width — the hashing a text model's own vocabulary is
/// held in. Only WHICH column a word is counted in comes from the digest; every value is a count
/// of the caller's own words, as a share of the whole, so a long text and a short one are fitted
/// in the same scale.
#[must_use]
pub fn text_features(text: &str) -> Vec<f64> {
    use ::sha2::Digest;

    let mut counts = vec![0.0_f64; TEXT_FEATURE_WIDTH];
    let mut words = 0.0_f64;
    for word in text.split(|character: char| !character.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let digest = ::sha2::Sha256::digest(word.to_lowercase().as_bytes());
        let mut taken = [0_u8; 8];
        taken.copy_from_slice(&digest[..8]);
        let bucket = (u64::from_be_bytes(taken) % TEXT_FEATURE_WIDTH as u64) as usize;
        counts[bucket] += 1.0;
        words += 1.0;
    }
    if words > 0.0 {
        for count in &mut counts {
            *count /= words;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One manifest's read, as the rows below drive it.
    fn read_of(body: &str, attributes: &[&str]) -> ChannelData {
        let named: Vec<String> = attributes.iter().map(|name| (*name).to_string()).collect();
        let mut read = ChannelData::new("train", "s3://bucket/manifest.jsonl".to_string());
        read_augmented_records(body.as_bytes(), &named, &mut read);
        read
    }

    /// RULE: a channel whose data type is the augmented manifest holds one record per JSON
    /// line, built from the attributes it named in order — the last the record's target, the
    /// ones before it its features — and a line that is not of that form is counted.
    ///
    /// The rows differ along the attribute count and the attribute names, along whether the
    /// channel names attributes at all, along the value form (text, a number, a numeric string,
    /// a list), and along what makes a line hold no record (not an object, a missing attribute,
    /// no data attribute).
    #[test]
    fn a_manifests_records_are_the_attributes_of_its_json_lines() {
        /// One row: the case, the manifest's lines, the attributes the channel names, and what
        /// the read must hold — the records, the lines that held none, and the feature width.
        struct Row {
            case: &'static str,
            body: &'static str,
            attributes: &'static [&'static str],
            records: usize,
            skipped: usize,
            width: usize,
        }
        let rows = [
            Row {
                case: "a text attribute and a numeric label",
                body: "{\"source\": \"alpha beta\", \"label\": 1}\n\
                       {\"source\": \"gamma delta\", \"label\": 2}\n",
                attributes: &["source", "label"],
                records: 2,
                skipped: 0,
                width: TEXT_FEATURE_WIDTH,
            },
            Row {
                case: "the attributes taken in the order the channel names them",
                body: "{\"label\": 1, \"source\": \"alpha\"}\n",
                attributes: &["source", "label"],
                records: 1,
                skipped: 0,
                width: TEXT_FEATURE_WIDTH,
            },
            Row {
                case: "numeric attributes only",
                body: "{\"x\": 1, \"y\": 2.5, \"target\": 7}\n\
                       {\"x\": 3, \"y\": 4, \"target\": 9}\n",
                attributes: &["x", "y", "target"],
                records: 2,
                skipped: 0,
                width: 2,
            },
            Row {
                case: "a label written as a string",
                body: "{\"x\": 1, \"target\": \"7\"}\n",
                attributes: &["x", "target"],
                records: 1,
                skipped: 0,
                width: 1,
            },
            Row {
                case: "a list attribute contributing its own values",
                body: "{\"vector\": [1, 2, 3], \"label\": 0}\n",
                attributes: &["vector", "label"],
                records: 1,
                skipped: 0,
                width: 3,
            },
            Row {
                case: "no attribute named, so every attribute of the line is taken",
                body: "{\"a\": 1, \"b\": 2}\n",
                attributes: &[],
                records: 1,
                skipped: 0,
                width: 1,
            },
            Row {
                case: "a line that is not a JSON object, and one missing a named attribute",
                body: "not json\n{\"source\": \"alpha\"}\n\
                       {\"source\": \"beta\", \"label\": 1}\n",
                attributes: &["source", "label"],
                records: 1,
                skipped: 2,
                width: TEXT_FEATURE_WIDTH,
            },
            Row {
                case: "a line whose only attribute is the label",
                body: "{\"label\": 1}\n",
                attributes: &["label"],
                records: 0,
                skipped: 1,
                width: 0,
            },
        ];
        for row in rows {
            let read = read_of(row.body, row.attributes);
            assert_eq!(
                (read.records.len(), read.skipped),
                (row.records, row.skipped),
                "case {:?}: expected {} record(s) and {} skipped, got {read:?}",
                row.case,
                row.records,
                row.skipped
            );
            for record in &read.records {
                assert_eq!(
                    record.features.len(),
                    row.width,
                    "case {:?}: expected {} feature(s), got {record:?}",
                    row.case,
                    row.width
                );
            }
        }
    }

    /// RULE: the label the record is fitted against is the attribute the channel named LAST,
    /// and a label that is text is the class it names, numbered in the order the classes
    /// appear.
    #[test]
    fn the_last_attribute_is_the_label_the_record_carries() {
        let read = read_of(
            "{\"source\": \"alpha\", \"label\": 4}\n{\"source\": \"beta\", \"label\": 9}\n",
            &["source", "label"],
        );
        let targets: Vec<f64> = read.records.iter().map(|record| record.target).collect();
        assert_eq!(
            targets,
            vec![4.0, 9.0],
            "case a numeric label: expected the label itself as the target, got {read:?}"
        );

        let read = read_of(
            "{\"source\": \"alpha\", \"class\": \"cat\"}\n\
             {\"source\": \"beta\", \"class\": \"dog\"}\n\
             {\"source\": \"gamma\", \"class\": \"cat\"}\n",
            &["source", "class"],
        );
        let targets: Vec<f64> = read.records.iter().map(|record| record.target).collect();
        assert_eq!(
            targets,
            vec![0.0, 1.0, 0.0],
            "case a named class: expected one number per class, in the order they appear, got \
             {read:?}"
        );

        // A label an attribute carries inside an object is read from the value inside it.
        let read = read_of(
            "{\"source-ref\": \"a b\", \"label-metadata\": {\"class-name\": \"cat\"}}\n\
             {\"source-ref\": \"c d\", \"label-metadata\": {\"class-name\": \"dog\"}}\n",
            &["source-ref", "label-metadata"],
        );
        let targets: Vec<f64> = read.records.iter().map(|record| record.target).collect();
        assert_eq!(
            targets,
            vec![0.0, 1.0],
            "case a nested label: expected the class inside the attribute, got {read:?}"
        );
    }

    /// RULE: the features a text attribute contributes are the frequency its own words fall
    /// into the buckets, so the same words give the same record and different words do not.
    #[test]
    fn a_texts_features_follow_its_words() {
        for (case, text) in [
            ("one word", "alpha"),
            ("several words", "alpha beta gamma"),
            ("punctuation between the words", "alpha, beta; gamma!"),
            ("no word at all", "   "),
        ] {
            let features = text_features(text);
            assert_eq!(
                features.len(),
                TEXT_FEATURE_WIDTH,
                "case {case:?}: expected {TEXT_FEATURE_WIDTH} columns, got {features:?}"
            );
            let total: f64 = features.iter().sum();
            let expected = if text.chars().any(char::is_alphanumeric) {
                1.0
            } else {
                0.0
            };
            assert!(
                (total - expected).abs() < 1e-9,
                "case {case:?}: the shares of a text's words must sum to {expected}, got \
                 {features:?}"
            );
        }
        assert_eq!(
            text_features("alpha beta"),
            text_features("BETA  alpha"),
            "the same words must give the same features whatever their order or case"
        );
        assert_ne!(
            text_features("alpha beta"),
            text_features("gamma delta"),
            "different words must give different features"
        );
    }

    /// RULE: the read is bounded — no more than [`MAX_RECORDS`] records, and no record wider
    /// than [`MAX_FEATURES`] features.
    #[test]
    fn the_read_is_bounded_in_width_and_in_length() {
        let wide: String = {
            let values: Vec<String> = (0..MAX_FEATURES + 8).map(|at| at.to_string()).collect();
            format!("{{\"vector\": [{}], \"label\": 1}}\n", values.join(","))
        };
        let read = read_of(&wide, &["vector", "label"]);
        let features = read.records.first().map_or(0, |record| record.features.len());
        assert_eq!(
            features, MAX_FEATURES,
            "case a record wider than the ceiling: expected {MAX_FEATURES} features, got \
             {features}"
        );

        let long: String = (0..MAX_RECORDS + 16)
            .map(|row| format!("{{\"x\": {row}, \"label\": 1}}\n"))
            .collect();
        let read = read_of(&long, &["x", "label"]);
        assert_eq!(
            read.records.len(),
            MAX_RECORDS,
            "case more lines than the ceiling: expected {MAX_RECORDS} records, got {}",
            read.records.len()
        );
    }
}
