// SPDX-License-Identifier: CC0-1.0

//! What an INCREMENTAL export is taken over, and what it writes.
//!
//! "Choice of whether to execute as a full export or incremental export"
//! (`ExportTableToPointInTime$ExportType`): a FULL export writes the table as it stood at
//! one point in time, while an INCREMENTAL one writes what CHANGED over a period — "the
//! table's state including and after" the period's start and "just prior to" its end
//! (`IncrementalExportSpecification$ExportFromTime` / `$ExportToTime`). The two kinds of
//! export therefore write different data, and this module holds what the second one is
//! taken over: the period a request asks for and how it is resolved and held to the rules
//! the surface keeps for it, and the change records the table's own continuous backups
//! answer it with ([`PointInTimeRecovery::changes_between`]).
//!
//! It lives beside [`super`] rather than inside it because it is one concern — the
//! incremental half of the export surface — and `super` re-exports it, so every caller
//! still names it `crate::state::…`.

use super::*;

/// The shortest period an incremental export may be taken over.
///
/// DynamoDB holds an incremental export to a period of at least fifteen minutes, and
/// refuses a shorter one with the sentence below. Named here because it is a rule of the
/// export surface rather than of either operation that reads the catalogue.
pub const MIN_INCREMENTAL_EXPORT_PERIOD: Duration = Duration::from_secs(15 * 60);

/// How a period shorter than [`MIN_INCREMENTAL_EXPORT_PERIOD`] is refused: the fixed
/// sentence DynamoDB answers such a request with, carried by the modelled
/// `InvalidExportTimeException`.
///
/// Fixed text with no variable part: the sentence states the rule the two members broke
/// and echoes neither of the instants they carried.
pub const INCREMENTAL_EXPORT_PERIOD_TOO_SHORT_MESSAGE: &str =
    "Difference between export period from time and export period to time is less than 15 minutes";

/// The `ExportViewType` an incremental export that names none is taken with: "The view
/// type that was chosen for the export. Valid values are NEW_AND_OLD_IMAGES and
/// NEW_IMAGES. The default value is NEW_AND_OLD_IMAGES"
/// (`IncrementalExportSpecification$ExportViewType`).
pub const DEFAULT_EXPORT_VIEW_TYPE: &str = "NEW_AND_OLD_IMAGES";

/// The other value that member declares, which writes "only the new (latest) image" of
/// each changed item.
pub const EXPORT_VIEW_TYPE_NEW_IMAGE: &str = "NEW_IMAGE";

/// How a refusal names the incremental specification and its members: the paths the
/// request validator reports a violation of them against, as the model spells them on the
/// wire (lowerCamel).
pub const INCREMENTAL_SPECIFICATION_MEMBER_PATH: &str = "incrementalExportSpecification";
pub const EXPORT_FROM_TIME_MEMBER_PATH: &str = "incrementalExportSpecification.exportFromTime";
pub const EXPORT_VIEW_TYPE_MEMBER_PATH: &str = "incrementalExportSpecification.exportViewType";

/// One change an incremental export writes: the item's key, the images on either side of
/// the export period, and the instant of the last write inside the period that touched it.
///
/// An item the period only CREATED carries no old image and one it only REMOVED carries no
/// new one, which is what tells the three kinds of change apart in the data an export
/// writes ([`change_record_json`]).
#[derive(Debug, Clone)]
pub struct ItemChange {
    /// When the last write inside the period landed on this item, which the record reports
    /// as the instant the change was written.
    pub at: SystemTime,
    /// The item's key as its table's key schema names it ([`Table::key_attributes`]).
    pub keys: Item,
    /// The item as it stood just before the period, absent where the period created it.
    pub old_image: Option<Item>,
    /// The item as it stood just before the period's end, absent where the period removed
    /// it.
    pub new_image: Option<Item>,
}

/// The data ONE export task writes, which is what the kind of export decides: the items of
/// a point in time for a FULL export, and the changes over a period for an INCREMENTAL
/// one.
///
/// Both are the table's OWN data, read off its items and the versions its continuous
/// backups kept; nothing here is synthesised, and an export whose period nothing was
/// written in writes an empty data file and a manifest that says so.
#[derive(Debug, Clone)]
pub enum ExportedData {
    /// The items the table held at the point in time a full export was taken at.
    Snapshot(Vec<Item>),
    /// What changed over the period an incremental export was taken over, oldest key
    /// first.
    Changes(Vec<ItemChange>),
}

impl ExportedData {
    /// "The number of items exported" (`ExportDescription$ItemCount`): the items a full
    /// export wrote, and the changed items an incremental one wrote.
    pub fn item_count(&self) -> usize {
        match self {
            Self::Snapshot(items) => items.len(),
            Self::Changes(changes) => changes.len(),
        }
    }

    /// The bytes of ITEM DATA this export renders, which is what the per-export ceiling is
    /// measured against ([`MAX_EXPORTED_BYTES`]): a change record carries the images on
    /// either side of the period, so both are counted.
    pub fn rendered_bytes(&self) -> i64 {
        match self {
            Self::Snapshot(items) => items
                .iter()
                .fold(0_i64, |total, item| total.saturating_add(item_size(item))),
            Self::Changes(changes) => changes.iter().fold(0_i64, |total, change| {
                let images = change
                    .old_image
                    .iter()
                    .chain(change.new_image.iter())
                    .fold(0_i64, |bytes, image| bytes.saturating_add(item_size(image)));
                total.saturating_add(images)
            }),
        }
    }
}

/// The incremental period a request STATED, as it stated it.
///
/// Held on the export's request record so a repeat under the same `ClientToken` can be told
/// from a changed one, which is why the members stay the `Option`s they arrived as: two
/// calls that both leave `ExportToTime` out ask for the same export, while the instant each
/// resolves it to moves with the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedIncrementalExport {
    pub export_from_time: Option<SystemTime>,
    pub export_to_time: Option<SystemTime>,
    pub export_view_type: Option<String>,
}

/// The period an incremental export is RESOLVED to: the instants its data is read between
/// and the view the records are written in, which is what the export's readers report as
/// its `IncrementalExportSpecification`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalExportWindow {
    /// "The inclusive start range for the export table's data" (`$ExportFromTime`).
    pub from: SystemTime,
    /// "The exclusive end range" (`$ExportToTime`), resolved to the newest exportable point
    /// where the request named none.
    pub to: SystemTime,
    /// The view the records are written in ([`DEFAULT_EXPORT_VIEW_TYPE`]).
    pub view_type: String,
}

impl IncrementalExportWindow {
    /// Whether this export writes the OLD image of a changed item as well as the new one,
    /// which is what the two view types differ in.
    pub fn carries_old_images(&self) -> bool {
        self.view_type != EXPORT_VIEW_TYPE_NEW_IMAGE
    }

    /// This period as the export readers report it
    /// (`ExportDescription$IncrementalExportSpecification`).
    pub fn describe(&self) -> crate::ops::types::IncrementalExportSpecification {
        crate::ops::types::IncrementalExportSpecification {
            export_from_time: Some(reported_date(self.from)),
            export_to_time: Some(reported_date(self.to)),
            export_view_type: Some(self.view_type.clone()),
        }
    }
}

/// Why an incremental export cannot be taken as it was asked for.
#[derive(Debug, Clone)]
pub enum IncrementalRefusal {
    /// A fault of the REQUEST's members — one the export cannot be taken without, or one it
    /// cannot be taken with — which is the `ValidationException` such an input is.
    Request(RequestError),
    /// A fault of the PERIOD it asked for: an instant outside the table's recovery window,
    /// or a period shorter than the minimum, each of which is the modelled
    /// `InvalidExportTimeException` with the sentence carried here.
    ExportPeriod(&'static str),
}

/// The period an export is taken over, resolved from what the request stated and held to
/// the rules this surface keeps for it — `None` for a FULL export, which is taken at one
/// point in time instead.
///
/// The rules, in the order they are settled:
///
/// * "If INCREMENTAL_EXPORT is provided, the IncrementalExportSpecification must also be
///   used" (`$ExportType`), so an incremental export that states no period is refused for
///   the member it left out; and the period is the incremental export's own, so a FULL
///   export that states one is refused for the member it cannot be taken with;
/// * `ExportFromTime` is the period's "inclusive start range", so an incremental export
///   that states none has no period at all and is refused for that member too, while
///   `ExportToTime` resolves to the newest exportable point — "if this is not provided, the
///   latest time with data available will be used" — and the view type to the value its own
///   member documents ([`DEFAULT_EXPORT_VIEW_TYPE`]);
/// * both instants must lie inside the table's recovery window — "you can export data from
///   any time within the point in time recovery window" (the operation's doc) — which is
///   `InvalidExportTimeException`;
/// * and the period must span at least [`MIN_INCREMENTAL_EXPORT_PERIOD`], which is that
///   same error with the sentence DynamoDB states that rule with.
pub fn resolved_incremental_window(
    export_type: &str,
    requested: Option<&RequestedIncrementalExport>,
    earliest: SystemTime,
    latest: SystemTime,
) -> Result<Option<IncrementalExportWindow>, IncrementalRefusal> {
    let incremental = export_type == EXPORT_TYPE_INCREMENTAL;
    let Some(requested) = requested else {
        return if incremental {
            Err(IncrementalRefusal::Request(RequestError::Validation(
                absent_member_message(INCREMENTAL_SPECIFICATION_MEMBER_PATH),
            )))
        } else {
            Ok(None)
        };
    };
    if !incremental {
        return Err(IncrementalRefusal::Request(RequestError::Validation(
            format!(
                "{INVALID_REQUEST_PREFIX}{}",
                validation_message([constraint_clause(
                    INCREMENTAL_SPECIFICATION_MEMBER_PATH,
                    None,
                    &format!(
                        "Member must only be stated for exportType {EXPORT_TYPE_INCREMENTAL}"
                    ),
                )])
                .unwrap_or_default()
            ),
        )));
    }
    // The view the records are written in, held to the value set the model declares for the
    // member and resolved to the one it documents for a request that names none.
    let view_type = match requested.export_view_type.as_deref() {
        Some(named) if !crate::ops::types::EXPORT_VIEW_TYPE_VALUES.contains(&named) => {
            return Err(IncrementalRefusal::Request(invalid_member(
                EXPORT_VIEW_TYPE_MEMBER_PATH,
                Some(named),
                &enum_value_requirement(crate::ops::types::EXPORT_VIEW_TYPE_VALUES),
            )))
        }
        Some(named) => named.to_string(),
        None => DEFAULT_EXPORT_VIEW_TYPE.to_string(),
    };
    let Some(from) = requested.export_from_time else {
        return Err(IncrementalRefusal::Request(RequestError::Validation(
            absent_member_message(EXPORT_FROM_TIME_MEMBER_PATH),
        )));
    };
    // Each instant is read back at the precision this service reports a timestamp at, so a
    // client that sends back a bound it read names exactly that bound rather than landing a
    // fraction outside it.
    let from = at_nearest_timestamp_precision(from);
    let to = requested
        .export_to_time
        .map_or(latest, at_nearest_timestamp_precision);
    if from < earliest || from > latest || to < earliest || to > latest {
        return Err(IncrementalRefusal::ExportPeriod(INVALID_EXPORT_TIME_MESSAGE));
    }
    // The period's own length, which a period that runs backwards fails as surely as one
    // that is merely short.
    let spans = to
        .duration_since(from)
        .unwrap_or(Duration::from_secs(0));
    if spans < MIN_INCREMENTAL_EXPORT_PERIOD {
        return Err(IncrementalRefusal::ExportPeriod(
            INCREMENTAL_EXPORT_PERIOD_TOO_SHORT_MESSAGE,
        ));
    }
    Ok(Some(IncrementalExportWindow {
        from,
        to,
        view_type,
    }))
}

/// One change as the data file of an incremental export carries it: the item's keys, the
/// images the view type asks for, and the metadata that says when the change was written.
///
/// A record of the period's effect on ONE item: an item the period created carries only a
/// new image, one it removed carries only an old one, and one it changed carries both —
/// which is how a consumer of the export tells the three apart. An export taken in the
/// `NEW_IMAGE` view writes no old image at all, whatever the change was.
pub fn change_record_json(change: &ItemChange, old_images: bool) -> ::serde_json::Value {
    let mut record = ::serde_json::Map::new();
    record.insert(
        "Metadata".to_string(),
        ::serde_json::json!({ "WriteTimestampMicros": write_timestamp_micros(change.at) }),
    );
    record.insert("Keys".to_string(), item_json(&change.keys));
    if let Some(new_image) = &change.new_image {
        record.insert("NewImage".to_string(), item_json(new_image));
    }
    if old_images {
        if let Some(old_image) = &change.old_image {
            record.insert("OldImage".to_string(), item_json(old_image));
        }
    }
    ::serde_json::Value::Object(record)
}

/// The instant a change record carries: epoch MICROSECONDS, as a string — the resolution
/// the metadata of an incremental export's record states a write at.
fn write_timestamp_micros(at: SystemTime) -> String {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_micros())
        .to_string()
}
