// SPDX-License-Identifier: CC0-1.0

//! Whether a position update a tracker receives is STORED, as that tracker's
//! `PositionFiltering` mode decides.
//!
//! Kept beside [`super`] rather than in one operation's file because the mode is a
//! property of the tracker RESOURCE: it is chosen when the tracker is created, read
//! back by every tracker read, and applied to EVERY write into a device's history, so
//! the predicate lives with the record it is stored on.
//!
//! A filtered update is dropped SILENTLY: the call that carried it still answers with
//! an empty per-entry error list, the update is simply not persisted, and the device's
//! last known position and its history stay at the sample stored before it. Because an
//! update that is not stored is also not evaluated against the geofence collections a
//! tracker consumes, [`super::Tracker::record_filtered_position`] is the one gate every
//! write passes through.

use super::{EpochMillis, StoredPosition, Tracker};

/// The mode a tracker stores at most one position per interval under, as the model's
/// `PositionFiltering` enum spells it. It is this service's default, so it is also what
/// a tracker whose mode was never chosen behaves as ([`super::DEFAULT_POSITION_FILTERING`]).
pub const POSITION_FILTERING_TIME_BASED: &str = super::DEFAULT_POSITION_FILTERING;

/// The mode a tracker stores a position under only once the device has moved far
/// enough, as the model's `PositionFiltering` enum spells it.
pub const POSITION_FILTERING_DISTANCE_BASED: &str = "DistanceBased";

/// The mode a tracker stores a position under only once the device has moved further
/// than the two samples' measured accuracy, as the model's `PositionFiltering` enum
/// spells it.
pub const POSITION_FILTERING_ACCURACY_BASED: &str = "AccuracyBased";

/// How far a `DistanceBased` tracker requires a device to have moved before it stores
/// another position, in metres.
///
/// The `CreateTrackerRequest$PositionFiltering` doc: "`DistanceBased` - If the device
/// has moved less than 30 m (98.4 ft), location updates are ignored." A device that has
/// moved exactly that far has not moved LESS than it, so the threshold is the smallest
/// movement that is still stored.
pub const DISTANCE_FILTER_METRES: f64 = 30.0;

/// How far apart a `TimeBased` tracker requires two stored samples' instants to be, in
/// milliseconds.
///
/// The `BatchUpdateDevicePosition` doc: "location data is stored at a maximum of one
/// position per 30 second interval. If your update frequency is more often than every
/// 30 seconds, only one update per 30 seconds is stored for each unique device ID." An
/// update exactly that far from what the device holds is not more frequent than the
/// interval, so the threshold is the smallest gap that is still stored.
pub const TIME_FILTER_MILLIS: EpochMillis = 30_000;

/// `PositionalAccuracy$Horizontal` assumed for an update that carries no `Accuracy`.
///
/// The `BatchUpdateDevicePosition` doc: "If `PositionFiltering` is set to
/// `AccuracyBased` filtering, Amazon Location uses the default value `{ "Horizontal":
/// 0}` when accuracy is not provided on a `DevicePositionUpdate`."
pub const DEFAULT_HORIZONTAL_ACCURACY_METRES: f64 = 0.0;

impl Tracker {
    /// Whether this tracker STORES `sample`, as its `PositionFiltering` mode decides.
    ///
    /// Each mode compares the update against what the device ALREADY holds, so a device
    /// with no stored sample is always stored and the comparison is never against
    /// another device's samples:
    ///
    /// * `TimeBased` stores at most one position per [`TIME_FILTER_MILLIS`] interval, so
    ///   an update whose instant is nearer than that to an instant the device holds is
    ///   dropped. The nearest held instant is the one compared against, so the stored
    ///   instants are that far apart however the updates arrived.
    /// * `DistanceBased` drops an update whose great-circle distance from the device's
    ///   last known position is under [`DISTANCE_FILTER_METRES`].
    /// * `AccuracyBased` drops an update whose great-circle distance from the device's
    ///   last known position is under the sum of that sample's and this update's
    ///   horizontal accuracy, each [`DEFAULT_HORIZONTAL_ACCURACY_METRES`] when the
    ///   update carried none.
    ///
    /// An unrecognised mode is read as the documented default, which is the mode a
    /// tracker created without the member carries.
    pub fn stores_position(&self, sample: &StoredPosition) -> bool {
        match self.position_filtering.as_str() {
            POSITION_FILTERING_DISTANCE_BASED => {
                self.moved_at_least(sample, DISTANCE_FILTER_METRES)
            }
            POSITION_FILTERING_ACCURACY_BASED => {
                let measured = self
                    .latest_position(&sample.device_id)
                    .map(|last| horizontal(last) + horizontal(sample))
                    .unwrap_or(DEFAULT_HORIZONTAL_ACCURACY_METRES);
                self.moved_at_least(sample, measured)
            }
            _ => self
                .nearest_sample_gap_millis(&sample.device_id, sample.sample_time)
                .is_none_or(|gap| gap >= TIME_FILTER_MILLIS),
        }
    }

    /// Record `sample` unless this tracker's `PositionFiltering` mode filters it out,
    /// reporting whether it was stored.
    ///
    /// The one write path into a device's history: a caller that accepted an update
    /// hands it here, and the answer is whether the device holds it — never an error,
    /// because a filtered update is not a failed one.
    pub fn record_filtered_position(&mut self, sample: StoredPosition) -> bool {
        if !self.stores_position(&sample) {
            return false;
        }
        self.record_position(sample);
        true
    }

    /// Whether `sample` lies at least `metres` from the device's last known position,
    /// which a device with no stored position trivially does.
    fn moved_at_least(&self, sample: &StoredPosition, metres: f64) -> bool {
        self.latest_position(&sample.device_id).is_none_or(|last| {
            // The crate's one great-circle arc, shared with the Places plane: the
            // shortest distance between two WGS 84 positions on the Earth.
            super::places::great_circle_metres(last.position, sample.position) >= metres
        })
    }

    /// How far `sample_time` is, in milliseconds, from the NEAREST instant this device
    /// already holds a sample at, or `None` when it holds none.
    ///
    /// The history is sorted ascending by sample time, so the two candidates are the
    /// instants either side of `sample_time` and the probe is logarithmic in the
    /// history's length.
    fn nearest_sample_gap_millis(
        &self,
        device_id: &str,
        sample_time: EpochMillis,
    ) -> Option<EpochMillis> {
        let history = self.devices.get(device_id)?;
        let at = history.partition_point(|held| held.sample_time < sample_time);
        let before = at.checked_sub(1).and_then(|index| history.get(index));
        let after = history.get(at);
        before
            .into_iter()
            .chain(after)
            .map(|held| held.sample_time.saturating_sub(sample_time).saturating_abs())
            .min()
    }
}

/// The horizontal accuracy a sample was reported with, in metres, defaulting as the
/// model documents for an update that carried none. A negative value cannot describe an
/// accuracy radius, so it contributes nothing to the movement a mode requires.
fn horizontal(sample: &StoredPosition) -> f64 {
    sample
        .accuracy
        .filter(|metres| metres.is_finite() && *metres > 0.0)
        .unwrap_or(DEFAULT_HORIZONTAL_ACCURACY_METRES)
}
