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

/// How a period whose two instants are out of ORDER is refused: the fixed sentence DynamoDB
/// answers such a request with, carried by the modelled `InvalidExportTimeException`.
///
/// The period runs FROM its "inclusive start range" TO its "exclusive end range"
/// (`$ExportFromTime` / `$ExportToTime`), so a start that is not strictly before the end
/// describes no period at all — a distinct defect from a period that merely runs SHORT
/// ([`INCREMENTAL_EXPORT_PERIOD_TOO_SHORT_MESSAGE`]), which is why it has its own sentence.
/// Fixed text with no variable part: it states the two members' order and echoes neither
/// instant.
pub const INCREMENTAL_EXPORT_PERIOD_ORDER_MESSAGE: &str =
    "Incremental export period from time should not be greater or equal to incremental export \
     period to time";

/// How an INCREMENTAL export that states no period at all is refused: the sentence
/// DynamoDB answers that request with, under the one it states a request it cannot act on
/// with ([`INVALID_REQUEST_PREFIX`]).
///
/// Fixed text with no variable part: it names the specification the export cannot be taken
/// without and the kind of export that requires it — "If INCREMENTAL_EXPORT is provided,
/// the IncrementalExportSpecification must also be used" (`$ExportType`) — rather than the
/// absent member's own wire path, because what is wrong is the PAIR the request stated.
pub const INCREMENTAL_SPECIFICATION_EXPECTED_MESSAGE: &str =
    "Incremental Export Specification expected when export type is set as Incremental Export";

/// How a FULL export that states an incremental period is refused: the sentence DynamoDB
/// answers that request with, under the same one.
///
/// Fixed text with no variable part, and AWS's own spacing before the comma: the period is
/// the incremental export's own, so the kind of export the request asked for is what makes
/// the member one it cannot carry.
pub const INCREMENTAL_SPECIFICATION_UNEXPECTED_MESSAGE: &str =
    "When Export Type is Full Export , Incremental Export Specification should not be provided";

/// How an incremental export whose period names no START is refused: the sentence DynamoDB
/// answers that request with, under the same one.
///
/// Fixed text with no variable part: it names the member the period cannot be resolved
/// without — the "inclusive start range" of the period (`$ExportFromTime`) — and the
/// specification it belongs to.
pub const EXPORT_FROM_TIME_EXPECTED_MESSAGE: &str =
    "ExportFromTime must be provided in IncrementalExportSpecification";

/// The `ExportViewType` an incremental export that names none is taken with: "The view
/// type that was chosen for the export. Valid values are NEW_AND_OLD_IMAGES and
/// NEW_IMAGES. The default value is NEW_AND_OLD_IMAGES"
/// (`IncrementalExportSpecification$ExportViewType`).
pub const DEFAULT_EXPORT_VIEW_TYPE: &str = "NEW_AND_OLD_IMAGES";

/// The other value that member declares, which writes "only the new (latest) image" of
/// each changed item.
pub const EXPORT_VIEW_TYPE_NEW_IMAGE: &str = "NEW_IMAGE";

/// How a refusal names a member of the incremental specification: the path the request
/// validator reports a violation of it against, as the model spells it on the wire
/// (lowerCamel).
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

/// How an incremental export whose period ENDS after the present is refused: the sentence
/// states the rule the window's end broke — a period cannot reach into the future, because
/// nothing has been written there yet — and names the instant the request asked for.
///
/// The fixed text is AWS's, with its own spacing before the instant; the variable part is
/// the `ExportToTime` the request carried, rendered as the UTC instant every timestamp of
/// this service is rendered as ([`reported_instant_text`]). It is the one value a caller
/// has to correct, so a refusal that named only the rule would leave it guessing which of
/// the period's two bounds it sent wrong.
pub fn export_to_time_in_future_message(requested: SystemTime) -> String {
    format!(
        "Incremental export period to time cannot be greater than the current time : {}",
        reported_instant_text(requested)
    )
}

/// How an incremental export whose period STARTS before the table's recoverable history is
/// refused: the sentence states the rule the window's start broke — a period cannot reach
/// back before the table had any data to change — and names the instant the request asked
/// for.
///
/// The fixed text is AWS's, with its own spacing before the instant; the variable part is
/// the `ExportFromTime` the request carried ([`reported_instant_text`]). The rule names the
/// table's creation because that is where the oldest restorable point of a table whose
/// recovery has been on since it was created stands
/// ([`PointInTimeRecovery::restorable_window`]).
pub fn export_from_time_before_history_message(requested: SystemTime) -> String {
    format!(
        "Incremental export period from time cannot be less than the table creation time : {}",
        reported_instant_text(requested)
    )
}

/// Why an incremental export cannot be taken as it was asked for.
#[derive(Debug, Clone)]
pub enum IncrementalRefusal {
    /// A fault of the REQUEST's members — one the export cannot be taken without, or one it
    /// cannot be taken with — which is the `ValidationException` such an input is.
    Request(RequestError),
    /// A fault of the PERIOD it asked for: an instant outside the table's recovery window,
    /// an end that reaches into the future, or a period shorter than the minimum, each of
    /// which is the modelled `InvalidExportTimeException` with the sentence carried here.
    ExportPeriod(String),
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
/// * the period's END may not reach past the present, and its START may not reach back
///   before the table's data does — "you can export data from any time within the point in
///   time recovery window" (the operation's doc), whose oldest point is where the table's
///   recoverable history begins. Each of the two is `InvalidExportTimeException` stating the
///   rule that bound broke and naming the instant the request sent for it
///   ([`export_to_time_in_future_message`], [`export_from_time_before_history_message`]);
/// * what is left outside the window — a start after its newest point, or an end before its
///   oldest — is that same error with the fixed sentence an instant no window covers earns
///   ([`INVALID_EXPORT_TIME_MESSAGE`]);
/// * and the period must RUN — its start strictly before its end
///   ([`INCREMENTAL_EXPORT_PERIOD_ORDER_MESSAGE`]) — and span at least
///   [`MIN_INCREMENTAL_EXPORT_PERIOD`], each that same error with the sentence DynamoDB
///   states that rule with.
pub fn resolved_incremental_window(
    export_type: &str,
    requested: Option<&RequestedIncrementalExport>,
    earliest: SystemTime,
    latest: SystemTime,
    now: SystemTime,
) -> Result<Option<IncrementalExportWindow>, IncrementalRefusal> {
    let incremental = export_type == EXPORT_TYPE_INCREMENTAL;
    let Some(requested) = requested else {
        return if incremental {
            Err(IncrementalRefusal::Request(RequestError::Validation(
                format!(
                    "{INVALID_REQUEST_PREFIX}{INCREMENTAL_SPECIFICATION_EXPECTED_MESSAGE}"
                ),
            )))
        } else {
            Ok(None)
        };
    };
    if !incremental {
        return Err(IncrementalRefusal::Request(RequestError::Validation(
            format!("{INVALID_REQUEST_PREFIX}{INCREMENTAL_SPECIFICATION_UNEXPECTED_MESSAGE}"),
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
            format!("{INVALID_REQUEST_PREFIX}{EXPORT_FROM_TIME_EXPECTED_MESSAGE}"),
        )));
    };
    // Each instant is read back at the precision this service reports a timestamp at, so a
    // client that sends back a bound it read names exactly that bound rather than landing a
    // fraction outside it.
    let from = at_nearest_timestamp_precision(from);
    let to = requested
        .export_to_time
        .map_or(latest, at_nearest_timestamp_precision);
    // The period ENDS at an instant the table has data up to, so an end past the present
    // asks for changes nothing has written yet. That is a rule about the CLOCK rather than
    // about the window the recovery offers — the window's newest point trails the present
    // ([`PointInTimeRecovery::restorable_window`]) — so it is asked first and states which
    // bound broke it and the instant that did ([`export_to_time_in_future_message`]).
    if to > now {
        return Err(IncrementalRefusal::ExportPeriod(
            export_to_time_in_future_message(to),
        ));
    }
    // The period STARTS at an instant the table has data from, so a start before the oldest
    // point the window reaches asks for changes made before the table had any history to
    // change — which is where that history begins ([`PointInTimeRecovery::restorable_window`]
    // derives it). It states that rule and the instant the request sent
    // ([`export_from_time_before_history_message`]).
    if from < earliest {
        return Err(IncrementalRefusal::ExportPeriod(
            export_from_time_before_history_message(from),
        ));
    }
    // The two instants' ORDER, which is what makes them a PERIOD at all: a start at or
    // after the end describes none, so that is answered before either instant is placed
    // against the window ([`INCREMENTAL_EXPORT_PERIOD_ORDER_MESSAGE`]).
    if from >= to {
        return Err(IncrementalRefusal::ExportPeriod(
            INCREMENTAL_EXPORT_PERIOD_ORDER_MESSAGE.to_string(),
        ));
    }
    // What is left is an end the window has not CAUGHT UP with: the newest restorable point
    // trails the present ([`PointInTimeRecovery::restorable_window`]), so an end between the
    // two is an instant no window covers yet even though it is not in the future, which is
    // the fixed sentence such an instant earns ([`INVALID_EXPORT_TIME_MESSAGE`]). A start
    // past that point carries its end with it, so this covers both.
    if to > latest {
        return Err(IncrementalRefusal::ExportPeriod(
            INVALID_EXPORT_TIME_MESSAGE.to_string(),
        ));
    }
    // The period's own length, which is asked of a period that HAS a length.
    let spans = to
        .duration_since(from)
        .unwrap_or(Duration::from_secs(0));
    if spans < MIN_INCREMENTAL_EXPORT_PERIOD {
        return Err(IncrementalRefusal::ExportPeriod(
            INCREMENTAL_EXPORT_PERIOD_TOO_SHORT_MESSAGE.to_string(),
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
