// SPDX-License-Identifier: CC0-1.0

//! The data quality PROFILE a ruleset evaluation run records, the statistics it holds, and
//! the anomaly-detection model trained over them.
//!
//! A run that evaluates rulesets over a table MEASURES things about that table on the way —
//! how many rows it has, how complete a column is ([`DqdlRuleMetric`]) — and the set of those
//! measurements one run took is a PROFILE of the dataset, addressed by the `ProfileId` the run
//! mints for it ([`new_data_quality_profile_id`]). Each measurement is a datapoint of a
//! STATISTIC, and a statistic outlives the profile that recorded one of its datapoints: the
//! same measurement of the same dataset keeps one `StatisticId`
//! ([`ComAmazonawsGlueAwsglueState::data_quality_statistic_id`]) across every run, because
//! what the anomaly-detection surface is about is one statistic's history.
//!
//! A model is trained over that history: `GetDataQualityModel` reports "the training status of
//! the data quality model" for a profile, and `GetDataQualityModelResult` reports what the
//! trained model says about one statistic — the value it predicts and the bounds a datapoint
//! outside which is an anomaly ([`DataQualityModel`]). The model is computed from the
//! datapoints that are stored, over the profiles a client has not annotated out of the
//! training set ([`DataQualityProfileEntry::inclusion_annotation`], which is what
//! `PutDataQualityProfileAnnotation` writes — "annotate all datapoints for a Profile").

use super::*;

/// Ceiling on the data quality profiles this account keeps.
///
/// This emulator's own capacity, NOT an AWS quota, for the reason every other ceiling here is
/// stated: a run is startable by any caller and each one records a profile. The OLDEST
/// profile is dropped first, so the datapoints a model is most likely to be read for are the
/// ones kept.
pub const MAX_DATA_QUALITY_PROFILES: usize = 1_000;

/// How many data quality profile ids this process has minted. A nonce source for
/// [`new_data_quality_profile_id`], exactly as [`MINTED_DATA_QUALITY_RESULT_IDS`] is for a
/// result id.
pub(super) static MINTED_DATA_QUALITY_PROFILE_IDS: AtomicU64 = AtomicU64::new(0);

/// The prefix a `ProfileId` carries, as the service spells one: the kind of thing the id
/// names, then the bits that make it unique.
pub const DATA_QUALITY_PROFILE_ID_PREFIX: &str = "dqprofile-";

/// Whether `profile_id` has the grammar a data quality profile id has
/// ([`DATA_QUALITY_PROFILE_ID_PREFIX`]).
///
/// The one question every operation of this surface asks before it looks anything up: a value
/// outside the grammar names no profile that could ever exist, so it is a malformed request
/// ([`messages::invalid_data_quality_profile_id`]) rather than a miss — the same reading the
/// data quality RUN surface takes of its own id ([`is_data_quality_run_id`]). One prefix, read
/// by the mint and by every read, so a minted id can never be one the reads reject.
#[must_use]
pub fn is_data_quality_profile_id(profile_id: &str) -> bool {
    profile_id.starts_with(DATA_QUALITY_PROFILE_ID_PREFIX)
}

/// A fresh `ProfileId`: the id the service assigns the profile a finished evaluation run
/// recorded.
///
/// [`DATA_QUALITY_PROFILE_ID_PREFIX`] followed by lowercase hexadecimal, which is the FORM
/// the service spells this id in; the width is this store's own — 128 bits, as every other id
/// it mints ([`new_data_quality_result_id`]) — because no width is published for it, and the
/// whole id is inside `HashString`'s `length 1..255`. The bits come from [`mint_hex_id`], so
/// two runs settling in the same nanosecond cannot be given one id, and the id is stored on
/// the profile, so every later read of it answers the same way.
///
/// This names a RESOURCE, not a credential: a credential would come from the credential
/// store, never from here.
pub fn new_data_quality_profile_id() -> String {
    format!(
        "{DATA_QUALITY_PROFILE_ID_PREFIX}{}",
        mint_hex_id(&MINTED_DATA_QUALITY_PROFILE_IDS, DATA_QUALITY_RUN_ID_WORDS)
    )
}

/// How many data quality statistic ids this process has minted. A nonce source for
/// [`new_data_quality_statistic_id`].
pub(super) static MINTED_DATA_QUALITY_STATISTIC_IDS: AtomicU64 = AtomicU64::new(0);

/// The prefix a `StatisticId` carries, as the service spells one.
pub const DATA_QUALITY_STATISTIC_ID_PREFIX: &str = "st-";

/// Whether `statistic_id` has the grammar a data quality statistic id has
/// ([`DATA_QUALITY_STATISTIC_ID_PREFIX`]).
///
/// The same question [`is_data_quality_profile_id`] asks of the profile id, asked of the OTHER
/// minted id this surface addresses things by: a value outside the grammar names no statistic
/// that could ever exist, so it is a malformed request
/// ([`messages::invalid_data_quality_statistic_id`]) rather than a miss, and it is the
/// STATISTIC that is reported malformed — never the other id of the same request. One prefix,
/// read by the mint ([`new_data_quality_statistic_id`]) and by every read, so a minted id can
/// never be one the reads reject.
#[must_use]
pub fn is_data_quality_statistic_id(statistic_id: &str) -> bool {
    statistic_id.starts_with(DATA_QUALITY_STATISTIC_ID_PREFIX)
}

/// A fresh `StatisticId`: the id the service assigns ONE measurement of one dataset the first
/// time a run measures it.
///
/// Spelled like the profile id above ([`new_data_quality_profile_id`]) under this id's own
/// prefix. It is minted ONCE per (dataset, measurement) pair and kept
/// ([`ComAmazonawsGlueAwsglueState::data_quality_statistic_ids`]), because a statistic is the
/// thing whose history a model is trained over: a second run measuring the same thing adds a
/// datapoint to that statistic rather than naming a new one.
pub fn new_data_quality_statistic_id() -> String {
    format!(
        "{DATA_QUALITY_STATISTIC_ID_PREFIX}{}",
        mint_hex_id(
            &MINTED_DATA_QUALITY_STATISTIC_IDS,
            DATA_QUALITY_RUN_ID_WORDS
        )
    )
}

/// `GetDataQualityModelResponse$Status` while the model over a profile's datapoints is still
/// being trained: of the modelled `DataQualityModelStatus` (`RUNNING`, `SUCCEEDED`,
/// `FAILED`), the one that says the training is under way.
pub const DATA_QUALITY_MODEL_STATUS_RUNNING: &str = "RUNNING";
/// See [`DATA_QUALITY_MODEL_STATUS_RUNNING`]: the status of a model that has been trained,
/// which is when its result can be read.
pub const DATA_QUALITY_MODEL_STATUS_SUCCEEDED: &str = "SUCCEEDED";
/// See [`DATA_QUALITY_MODEL_STATUS_RUNNING`]: the status of a training that produced no
/// model, which is what a profile holding no datapoint to train on leaves.
pub const DATA_QUALITY_MODEL_STATUS_FAILED: &str = "FAILED";

/// How long the model over a freshly recorded profile's datapoints takes to train.
///
/// CHOSEN, not documented: Glue publishes no duration for training an anomaly-detection
/// model, so this is the emulator's own span — the one it gives a transition the clock owes
/// ([`JOB_RUN_SETTLES_AFTER`] is the same span for a job run), deliberately not zero, so a
/// caller that reads the model straight after the run that recorded the profile still
/// observes the training in flight. The due instant is stored on the profile and materialised
/// by whichever later call reads it; nothing sleeps.
pub const DATA_QUALITY_MODEL_TRAINS_IN: Duration = JOB_RUN_SETTLES_AFTER;

/// `InclusionAnnotationValue` that keeps a profile's datapoints OUT of the training set: of
/// the modelled enum (`INCLUDE`, `EXCLUDE`), the excluding one.
pub const INCLUSION_ANNOTATION_EXCLUDE: &str = "EXCLUDE";

/// One datapoint a profile holds: what one measurement of the dataset answered, under the
/// statistic that measurement belongs to.
#[derive(Debug, Clone)]
pub struct ProfileStatistic {
    /// The `StatisticId` of the statistic this is a datapoint OF, minted once per (dataset,
    /// measurement) pair ([`new_data_quality_statistic_id`]).
    pub statistic_id: String,
    /// The name the measurement is reported by ([`DqdlRuleMetric::name`]), which is what makes
    /// one measurement of a dataset the same statistic across runs.
    pub name: String,
    /// The value this run measured.
    pub value: f64,
}

/// One stored data quality profile: the measurements ONE finished evaluation run took of its
/// dataset, and the training the model over them is in.
#[derive(Debug, Clone)]
pub struct DataQualityProfileEntry {
    /// `ProfileId`, minted when the run that recorded this profile finished
    /// ([`new_data_quality_profile_id`]), and the id every operation of this surface
    /// addresses the profile by.
    pub id: String,
    /// The evaluation run that recorded it, so a profile names where its datapoints came
    /// from.
    pub run_id: String,
    /// The dataset profiled, as the run addressed it — which is what makes two profiles'
    /// datapoints datapoints of the same statistics.
    pub table: EvaluatedTable,
    /// When the run that recorded this profile completed, which is when these measurements
    /// were taken. Reported as the model training's `StartedOn`, because the model over a
    /// profile's datapoints is trained from the moment they exist.
    pub recorded_on: SystemTime,
    /// The instant the clock owes this profile's model its trained status
    /// ([`DATA_QUALITY_MODEL_TRAINS_IN`] past [`Self::recorded_on`]).
    pub model_trained_at: SystemTime,
    /// The `InclusionAnnotation` a `PutDataQualityProfileAnnotation` wrote for this profile,
    /// or `None` while none has: "annotate all datapoints for a Profile", which is what keeps
    /// them in or out of what a model is trained on ([`Self::is_trained_on`]).
    pub inclusion_annotation: Option<String>,
    /// The datapoints this profile holds, in the order the run measured them.
    pub statistics: Vec<ProfileStatistic>,
}

impl DataQualityProfileEntry {
    /// The profile a finished run recorded: the measurements `statistics` under a freshly
    /// minted id, taken at `recorded_on`.
    pub fn of(
        run_id: &str,
        table: EvaluatedTable,
        statistics: Vec<ProfileStatistic>,
        recorded_on: SystemTime,
    ) -> Self {
        Self {
            id: new_data_quality_profile_id(),
            run_id: run_id.to_string(),
            table,
            recorded_on,
            model_trained_at: recorded_on
                .checked_add(DATA_QUALITY_MODEL_TRAINS_IN)
                .unwrap_or(recorded_on),
            // Nothing has annotated these datapoints: a profile carries an annotation only
            // once a client has written one.
            inclusion_annotation: None,
            statistics,
        }
    }

    /// Whether this profile's datapoints are part of what a model is TRAINED on: every
    /// profile is, until a client annotates it out of the set
    /// ([`INCLUSION_ANNOTATION_EXCLUDE`]).
    pub fn is_trained_on(&self) -> bool {
        self.inclusion_annotation.as_deref() != Some(INCLUSION_ANNOTATION_EXCLUDE)
    }

    /// The datapoint this profile holds for the statistic `statistic_id`, if it holds one.
    pub fn statistic(&self, statistic_id: &str) -> Option<&ProfileStatistic> {
        self.statistics
            .iter()
            .find(|statistic| statistic.statistic_id == statistic_id)
    }

    /// The training status of the model over this profile's datapoints at `now`, with the
    /// instant the training completed and, where it produced no model, the reason.
    ///
    /// Read off what is stored rather than recorded by a write: the training was due at
    /// [`Self::model_trained_at`] whether or not anybody asked, so a read reports
    /// [`DATA_QUALITY_MODEL_STATUS_RUNNING`] before that instant and the trained status from
    /// it. A profile holding no datapoint has nothing to train a model on, which is a
    /// training that produced none ([`DATA_QUALITY_MODEL_STATUS_FAILED`]) and reports
    /// [`messages::DATA_QUALITY_MODEL_HAS_NO_STATISTICS`] as its `FailureReason`.
    pub fn model_training(&self, now: SystemTime) -> DataQualityModelTraining {
        if self.model_trained_at > now {
            return DataQualityModelTraining {
                status: DATA_QUALITY_MODEL_STATUS_RUNNING,
                started_on: self.recorded_on,
                completed_on: None,
                failure_reason: None,
            };
        }
        if self.statistics.is_empty() {
            return DataQualityModelTraining {
                status: DATA_QUALITY_MODEL_STATUS_FAILED,
                started_on: self.recorded_on,
                completed_on: Some(self.model_trained_at),
                failure_reason: Some(messages::DATA_QUALITY_MODEL_HAS_NO_STATISTICS),
            };
        }
        DataQualityModelTraining {
            status: DATA_QUALITY_MODEL_STATUS_SUCCEEDED,
            started_on: self.recorded_on,
            completed_on: Some(self.model_trained_at),
            failure_reason: None,
        }
    }
}

/// What a read of the model over one profile's datapoints reports: the members
/// `GetDataQualityModel` answers with, resolved from what is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataQualityModelTraining {
    /// One of the modelled `DataQualityModelStatus` values.
    pub status: &'static str,
    /// "The timestamp when the data quality model training started."
    pub started_on: SystemTime,
    /// "The timestamp when the data quality model training completed", absent while it is
    /// still under way.
    pub completed_on: Option<SystemTime>,
    /// "The training failure reason", absent for a training that produced a model.
    pub failure_reason: Option<&'static str>,
}

impl DataQualityModelTraining {
    /// Whether the training has produced a model, i.e. there is a model RESULT to read
    /// ([`ComAmazonawsGlueAwsglueState::data_quality_model_result`]).
    pub fn produced_a_model(&self) -> bool {
        self.status == DATA_QUALITY_MODEL_STATUS_SUCCEEDED
    }
}

/// What the model trained over one statistic's history says about ONE of its datapoints: the
/// modelled `StatisticModelResult`, computed from the datapoints that are stored.
#[derive(Debug, Clone)]
pub struct DataQualityModel {
    /// "The predicted value": what the model expects this statistic to measure, which is the
    /// mean of the datapoints it was trained on.
    pub predicted_value: f64,
    /// "The lower bound" and "the upper bound" a datapoint outside which is an anomaly: the
    /// predicted value either side of the widest deviation from it the training set holds, so
    /// every datapoint the model was trained on lies inside the bounds it predicts and one
    /// further out than any of them does not.
    pub lower_bound: f64,
    /// See [`Self::lower_bound`].
    pub upper_bound: f64,
    /// "The actual value": the datapoint this result is about.
    pub actual_value: f64,
    /// "The date": when the profile holding that datapoint was recorded.
    pub date: SystemTime,
    /// "The inclusion annotation" of the profile holding it, so a client reading the model
    /// sees which datapoints were annotated out of the training set.
    pub inclusion_annotation: Option<String>,
}

/// The data quality profiles a finished run recorded, and the models trained over them.
impl ComAmazonawsGlueAwsglueState {
    /// The stored profile with this id, or `None` when this account holds no such profile.
    ///
    /// A profile is ADDRESSED by the id the run that recorded it minted, so the lookup is a
    /// walk over the profiles this account holds, bounded by [`MAX_DATA_QUALITY_PROFILES`].
    pub fn data_quality_profile(&self, id: &str) -> Option<&DataQualityProfileEntry> {
        self.data_quality_profiles
            .iter()
            .find(|profile| profile.id == id)
    }

    /// The same profile, for the one call that WRITES one
    /// (`PutDataQualityProfileAnnotation`): addressed exactly as the read addresses it.
    pub fn data_quality_profile_mut(
        &mut self,
        id: &str,
    ) -> Option<&mut DataQualityProfileEntry> {
        self.data_quality_profiles
            .iter_mut()
            .find(|profile| profile.id == id)
    }

    /// The `StatisticId` of the measurement `name` of the dataset `table`, minting it the
    /// first time that pair is measured.
    ///
    /// One id per (dataset, measurement) pair, kept in
    /// [`Self::data_quality_statistic_ids`]: a statistic is the thing whose history a model
    /// is trained over, so every run measuring the same thing about the same table
    /// contributes a datapoint to ONE statistic. The key folds the two catalog names the way
    /// the catalog stores them, so a run that addressed the table in another casing measures
    /// the same statistic.
    pub fn data_quality_statistic_id(&mut self, table: &EvaluatedTable, name: &str) -> String {
        let key = format!(
            "{}/{}/{}/{name}",
            table.catalog_id,
            fold_name(&table.database_name),
            fold_name(&table.table_name)
        );
        if let Some(stored) = self.data_quality_statistic_ids.get(&key) {
            return stored.clone();
        }
        let minted = new_data_quality_statistic_id();
        // Bounded like every other map this store grows per request: past the ceiling the id
        // is still answered, but it is not kept, so a statistic measured once more mints
        // another id rather than growing the map without end.
        if self.data_quality_statistic_ids.len() < MAX_DATA_QUALITY_PROFILES {
            self.data_quality_statistic_ids
                .insert(key, minted.clone());
        }
        minted
    }

    /// Keep the profile `profile`, dropping the OLDEST once this account holds more than
    /// [`MAX_DATA_QUALITY_PROFILES`].
    pub fn record_data_quality_profile(&mut self, profile: DataQualityProfileEntry) {
        self.data_quality_profiles.push(profile);
        if self.data_quality_profiles.len() > MAX_DATA_QUALITY_PROFILES {
            let overflow = self.data_quality_profiles.len() - MAX_DATA_QUALITY_PROFILES;
            self.data_quality_profiles.drain(..overflow);
        }
    }

    /// What the model trained over the statistic `statistic_id` says about every datapoint of
    /// it this account holds, newest profile last, or an EMPTY list when no stored profile
    /// holds a datapoint of that statistic.
    ///
    /// The model is computed here, from the datapoints that are stored: the training set is
    /// the datapoints of the profiles a client has not annotated out of it
    /// ([`DataQualityProfileEntry::is_trained_on`]), the predicted value is their mean and the
    /// bounds are that mean either side of the widest deviation from it the set holds. So
    /// annotating a profile out of the training set moves what the model predicts, which is
    /// what the annotation is for, and a statistic whose every profile is annotated out has a
    /// model trained on nothing and no result to report.
    pub fn data_quality_model_result(&self, statistic_id: &str) -> Vec<DataQualityModel> {
        let datapoints: Vec<&DataQualityProfileEntry> = self
            .data_quality_profiles
            .iter()
            .filter(|profile| profile.statistic(statistic_id).is_some())
            .collect();
        let trained_on: Vec<f64> = datapoints
            .iter()
            .filter(|profile| profile.is_trained_on())
            .filter_map(|profile| profile.statistic(statistic_id))
            .map(|statistic| statistic.value)
            .collect();
        if trained_on.is_empty() {
            return Vec::new();
        }
        let predicted_value = trained_on.iter().sum::<f64>() / trained_on.len() as f64;
        let deviation = trained_on
            .iter()
            .map(|value| (value - predicted_value).abs())
            .fold(0.0_f64, f64::max);
        datapoints
            .iter()
            .filter_map(|profile| {
                let statistic = profile.statistic(statistic_id)?;
                Some(DataQualityModel {
                    predicted_value,
                    lower_bound: predicted_value - deviation,
                    upper_bound: predicted_value + deviation,
                    actual_value: statistic.value,
                    date: profile.recorded_on,
                    inclusion_annotation: profile.inclusion_annotation.clone(),
                })
            })
            .collect()
    }

    /// The profile a finished evaluation run recorded.
    ///
    /// The datapoints are the metrics the run's own evaluation measured over the rows it read
    /// ([`EvaluatedDataQualityRule::metrics`]), across every ruleset it evaluated: a rule
    /// whose type measures a metric of the data contributes that measurement, and one decided
    /// off the schema alone contributes none — so a run that measured nothing records a
    /// profile holding no datapoint, which is a profile whose model has nothing to train on
    /// rather than no profile at all. Where two rules of a run measured the SAME thing, the
    /// profile holds one datapoint of that statistic — the first, since both read the same
    /// rows.
    ///
    /// Called while the store's guard is held, as the run is settled
    /// ([`Self::settle_due_data_quality_ruleset_evaluation_runs`]), so a profile exists from
    /// the instant the run that took its measurements finished.
    pub(super) fn profile_of_settled_run(
        &mut self,
        run_id: &str,
        table: EvaluatedTable,
        measured: &[(String, f64)],
        recorded_on: SystemTime,
    ) -> DataQualityProfileEntry {
        let mut statistics: Vec<ProfileStatistic> = Vec::new();
        for (name, value) in measured {
            let statistic_id = self.data_quality_statistic_id(&table, name);
            if statistics
                .iter()
                .any(|statistic| statistic.statistic_id == statistic_id)
            {
                continue;
            }
            statistics.push(ProfileStatistic {
                statistic_id,
                name: name.clone(),
                value: *value,
            });
        }
        DataQualityProfileEntry::of(run_id, table, statistics, recorded_on)
    }
}
