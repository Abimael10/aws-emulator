// SPDX-License-Identifier: CC0-1.0

//! WHICH encryption a query's results may be written under: the options a result
//! configuration can name, and the FLOOR a workgroup puts under them.
//!
//! `EncryptionConfiguration$EncryptionOption` declares the three
//! (`smithy_query(query="shape",shape="EncryptionOption")`): "Amazon S3 server-side
//! encryption with Amazon S3-managed keys (`SSE_S3`), server-side encryption with
//! KMS-managed keys (`SSE_KMS`), or client-side encryption with KMS-managed keys
//! (`CSE_KMS`)". Which one a query runs under is a CLIENT-side setting the workgroup can
//! both override and bound, and the two are different settings:
//!
//! * `EnforceWorkGroupConfiguration` REPLACES the client's whole result configuration with
//!   the workgroup's, so the option the request named is not used at all
//!   ([`crate::state::resolved_result_configuration`]).
//! * `EnableMinimumEncryptionConfiguration` "enforces a minimal level of encryption for the
//!   workgroup for query and calculation results that are written to Amazon S3. When
//!   enabled, workgroup users can set encryption only to the minimum level set by the
//!   administrator or higher when they submit queries." So the client still chooses, within
//!   a floor: a submission naming an option BELOW the workgroup's is refused
//!   ([`minimum_encryption_refusal`]), and one naming none is written under the workgroup's
//!   own ([`floors_encryption`]), because a floor nothing satisfies would otherwise write
//!   results with no encryption at all.
//!
//! The first takes precedence over the second: "if `EnforceWorkGroupConfiguration` is true,
//! the `EnableMinimumEncryptionConfiguration` flag is ignored, and the workgroup
//! configuration for encryption is used"
//! (`WorkGroupConfiguration$EnableMinimumEncryptionConfiguration`).
//!
//! HOW the option that comes out of all this is carried into the object store is
//! [`super::query_results`]'s: this module decides WHICH option a query runs under, and
//! nothing here writes anything.

use super::WorkGroupRecord;
use crate::ops::types::EncryptionConfiguration;

/// The `EncryptionOption` that names "Amazon S3 server-side encryption with Amazon
/// S3-managed keys" (`EncryptionConfiguration$EncryptionOption`), the first of the three
/// values that enum declares (`smithy_query(query="shape",shape="EncryptionOption")` reports
/// `[SSE_S3, SSE_KMS, CSE_KMS]`, the list [`crate::ops::types::ENCRYPTION_OPTION_VALUES`]
/// carries).
pub const ENCRYPTION_OPTION_SSE_S3: &str = "SSE_S3";

/// The `EncryptionOption` that names "server-side encryption with KMS-managed keys" — see
/// [`ENCRYPTION_OPTION_SSE_S3`].
pub const ENCRYPTION_OPTION_SSE_KMS: &str = "SSE_KMS";

/// The `EncryptionOption` that names "client-side encryption with KMS-managed keys" — see
/// [`ENCRYPTION_OPTION_SSE_S3`].
pub const ENCRYPTION_OPTION_CSE_KMS: &str = "CSE_KMS";

/// Every `EncryptionOption`, in the order a workgroup's minimum-encryption floor RANKS them:
/// the least encryption first.
///
/// The ranking is what the three options themselves say
/// (`EncryptionConfiguration$EncryptionOption`): results encrypted with keys Amazon S3
/// manages for itself, then with keys KMS manages for the account, then with those same
/// KMS-managed keys applied by the CLIENT before the results reach Amazon S3 at all. A floor
/// admits the option it names and every option after it in this order, which is also the
/// order [`minimum_encryption_refusal`] lists the admitted ones in.
pub const ENCRYPTION_OPTIONS_BY_LEVEL: [&str; 3] = [
    ENCRYPTION_OPTION_SSE_S3,
    ENCRYPTION_OPTION_SSE_KMS,
    ENCRYPTION_OPTION_CSE_KMS,
];

/// How much encryption `option` asks for, as the floor ranks it
/// ([`ENCRYPTION_OPTIONS_BY_LEVEL`]) — `None` for a value that is no `EncryptionOption` at
/// all, which the front door refuses before any handler sees it
/// (`crate::service::input_validation`).
fn encryption_level(option: &str) -> Option<usize> {
    ENCRYPTION_OPTIONS_BY_LEVEL
        .iter()
        .position(|declared| *declared == option)
}

/// The options a workgroup whose floor is `minimum` ADMITS: that option and every one that
/// asks for more encryption than it.
fn admitted_options(minimum: &str) -> Vec<&'static str> {
    let Some(level) = encryption_level(minimum) else {
        return Vec::new();
    };
    ENCRYPTION_OPTIONS_BY_LEVEL[level..].to_vec()
}

/// The `EncryptionOption` the workgroup `record` floors its results at, or `None` when it
/// floors nothing: a workgroup whose `EnableMinimumEncryptionConfiguration` is not set, one
/// whose settings override the client's altogether (the flag "is ignored" then), and one
/// whose own result configuration names no encryption to be the floor.
fn minimum_encryption_option(record: Option<&WorkGroupRecord>) -> Option<&str> {
    let configuration = &record?.configuration;
    if configuration.enforce_work_group_configuration == Some(true) {
        return None;
    }
    if configuration.enable_minimum_encryption_configuration != Some(true) {
        return None;
    }
    Some(
        configuration
            .result_configuration
            .as_ref()?
            .encryption_configuration
            .as_ref()?
            .encryption_option
            .as_str(),
    )
}

/// Whether the workgroup `record` puts a FLOOR under the encryption of its results, which is
/// what makes a submission naming no encryption of its own run under the workgroup's.
pub fn floors_encryption(record: Option<&WorkGroupRecord>) -> bool {
    minimum_encryption_option(record).is_some()
}

/// Why the workgroup `record` refuses a submission whose result configuration is
/// `requested`, or `None` for one it admits.
///
/// A submission is refused only for naming an option BELOW the floor: one that names the
/// floor or more encryption than it runs, and one that names no encryption at all is written
/// under the workgroup's own option instead ([`floors_encryption`]), so there is nothing
/// below the floor to refuse.
pub fn minimum_encryption_refusal(
    requested: Option<&EncryptionConfiguration>,
    record: Option<&WorkGroupRecord>,
) -> Option<String> {
    let minimum = minimum_encryption_option(record)?;
    let requested = requested?.encryption_option.as_str();
    (encryption_level(requested)? < encryption_level(minimum)?)
        .then(|| below_minimum_encryption(minimum))
}

/// How Athena words the refusal of an option below the floor: the rule, and then the options
/// the workgroup does admit.
///
/// The fixed text states the rule; the variable part is the list of options the floor admits
/// ([`admitted_options`]), which is what the caller has to choose from — rendered in the
/// order the floor ranks them, the floor itself first.
pub fn below_minimum_encryption(minimum: &str) -> String {
    format!(
        "The encryption option in the request does not meet the minimum encryption option \
         specified in the workgroup. The valid option(s) are: [{}].",
        admitted_options(minimum).join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{ResultConfiguration, WorkGroupConfiguration};
    use std::time::SystemTime;

    /// A workgroup whose result configuration names `minimum` as its encryption, with the
    /// two switches set to `floor` and `enforce`.
    fn work_group(
        minimum: Option<&str>,
        floor: Option<bool>,
        enforce: Option<bool>,
    ) -> WorkGroupRecord {
        WorkGroupRecord {
            name: "wg".to_string(),
            state: crate::state::WORK_GROUP_STATE_ENABLED.to_string(),
            description: None,
            configuration: WorkGroupConfiguration {
                result_configuration: Some(ResultConfiguration {
                    output_location: Some("s3://results/athena/".to_string()),
                    encryption_configuration: minimum.map(|option| EncryptionConfiguration {
                        encryption_option: option.to_string(),
                        kms_key: None,
                    }),
                    ..ResultConfiguration::default()
                }),
                enable_minimum_encryption_configuration: floor,
                enforce_work_group_configuration: enforce,
                ..WorkGroupConfiguration::default()
            },
            created_at: SystemTime::UNIX_EPOCH,
            tags: Vec::new(),
        }
    }

    /// One submitted encryption configuration, naming `option`.
    fn requested(option: &str) -> EncryptionConfiguration {
        EncryptionConfiguration {
            encryption_option: option.to_string(),
            kms_key: None,
        }
    }

    /// One row of the floor table: the workgroup's settings, the option the request named,
    /// and whether the floor refuses it.
    struct FloorRow {
        case: &'static str,
        /// The `EncryptionOption` the workgroup's own result configuration names, or `None`
        /// for a workgroup whose configuration names no encryption to be the floor.
        minimum: Option<&'static str>,
        /// `EnableMinimumEncryptionConfiguration`, as the record holds it.
        floor: Option<bool>,
        /// `EnforceWorkGroupConfiguration`, which takes precedence over it.
        enforce: Option<bool>,
        /// Whether a workgroup is resolved at all: a query may run in one this registry
        /// holds no record of.
        held: bool,
        /// The `EncryptionOption` the request named, or `None` for one naming none.
        asked: Option<&'static str>,
        /// Whether the submission is refused.
        refused: bool,
    }

    /// A row at `minimum` with both switches off and a request naming `asked`.
    fn row(case: &'static str, minimum: &'static str, asked: Option<&'static str>) -> FloorRow {
        FloorRow {
            case,
            minimum: Some(minimum),
            floor: None,
            enforce: None,
            held: true,
            asked,
            refused: false,
        }
    }

    /// A workgroup's floor admits the option it names and every option above it, refuses
    /// every option below it, and is ignored where the workgroup's settings override the
    /// client's altogether.
    #[test]
    fn a_floor_admits_its_own_option_and_every_stronger_one() {
        // The rows differ along where the floor sits, which option was asked for, which of
        // the two switches is set, and whether a workgroup is resolved at all.
        let rows: [FloorRow; 12] = [
            row(
                "no floor flag at all",
                ENCRYPTION_OPTION_SSE_KMS,
                Some(ENCRYPTION_OPTION_SSE_S3),
            ),
            FloorRow {
                floor: Some(false),
                ..row(
                    "the floor flag off",
                    ENCRYPTION_OPTION_SSE_KMS,
                    Some(ENCRYPTION_OPTION_SSE_S3),
                )
            },
            FloorRow {
                minimum: None,
                floor: Some(true),
                ..row(
                    "a floor the workgroup names no encryption for",
                    ENCRYPTION_OPTION_SSE_KMS,
                    Some(ENCRYPTION_OPTION_SSE_S3),
                )
            },
            FloorRow {
                floor: Some(true),
                refused: true,
                ..row(
                    "an option below the floor",
                    ENCRYPTION_OPTION_SSE_KMS,
                    Some(ENCRYPTION_OPTION_SSE_S3),
                )
            },
            FloorRow {
                floor: Some(true),
                ..row(
                    "the floor's own option",
                    ENCRYPTION_OPTION_SSE_KMS,
                    Some(ENCRYPTION_OPTION_SSE_KMS),
                )
            },
            FloorRow {
                floor: Some(true),
                ..row(
                    "an option above the floor",
                    ENCRYPTION_OPTION_SSE_KMS,
                    Some(ENCRYPTION_OPTION_CSE_KMS),
                )
            },
            FloorRow {
                floor: Some(true),
                ..row(
                    "the weakest floor admits every option",
                    ENCRYPTION_OPTION_SSE_S3,
                    Some(ENCRYPTION_OPTION_SSE_S3),
                )
            },
            FloorRow {
                floor: Some(true),
                refused: true,
                ..row(
                    "the strongest floor refuses the server-side ones",
                    ENCRYPTION_OPTION_CSE_KMS,
                    Some(ENCRYPTION_OPTION_SSE_KMS),
                )
            },
            FloorRow {
                floor: Some(true),
                ..row(
                    "the strongest floor admits its own option",
                    ENCRYPTION_OPTION_CSE_KMS,
                    Some(ENCRYPTION_OPTION_CSE_KMS),
                )
            },
            FloorRow {
                floor: Some(true),
                ..row(
                    "a request naming no encryption",
                    ENCRYPTION_OPTION_SSE_KMS,
                    None,
                )
            },
            FloorRow {
                floor: Some(true),
                enforce: Some(true),
                ..row(
                    "the workgroup's settings override the client's",
                    ENCRYPTION_OPTION_SSE_KMS,
                    Some(ENCRYPTION_OPTION_SSE_S3),
                )
            },
            FloorRow {
                floor: Some(true),
                held: false,
                ..row(
                    "no workgroup record at all",
                    ENCRYPTION_OPTION_SSE_KMS,
                    Some(ENCRYPTION_OPTION_SSE_S3),
                )
            },
        ];

        for case in rows {
            let record = case
                .held
                .then(|| work_group(case.minimum, case.floor, case.enforce));
            let answer =
                minimum_encryption_refusal(case.asked.map(requested).as_ref(), record.as_ref());
            assert_eq!(
                answer.is_some(),
                case.refused,
                "case {:?}: expected refused={}, got {answer:?}",
                case.case,
                case.refused
            );
            let in_force = case.held
                && case.minimum.is_some()
                && case.floor == Some(true)
                && case.enforce != Some(true);
            assert_eq!(
                floors_encryption(record.as_ref()),
                in_force,
                "case {:?}: the floor is in force exactly when the workgroup names one",
                case.case
            );
        }
    }

    /// The refusal lists the options the floor admits, floor first.
    #[test]
    fn the_refusal_lists_the_options_the_floor_admits() {
        // (the floor's option, the sentence it is refused with)
        let rows: [(&str, &str); 3] = [
            (
                ENCRYPTION_OPTION_SSE_S3,
                "The encryption option in the request does not meet the minimum encryption \
                 option specified in the workgroup. The valid option(s) are: [SSE_S3, SSE_KMS, \
                 CSE_KMS].",
            ),
            (
                ENCRYPTION_OPTION_SSE_KMS,
                "The encryption option in the request does not meet the minimum encryption \
                 option specified in the workgroup. The valid option(s) are: [SSE_KMS, \
                 CSE_KMS].",
            ),
            (
                ENCRYPTION_OPTION_CSE_KMS,
                "The encryption option in the request does not meet the minimum encryption \
                 option specified in the workgroup. The valid option(s) are: [CSE_KMS].",
            ),
        ];

        for (minimum, expected) in rows {
            let actual = below_minimum_encryption(minimum);
            assert_eq!(
                actual, expected,
                "case {minimum:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
