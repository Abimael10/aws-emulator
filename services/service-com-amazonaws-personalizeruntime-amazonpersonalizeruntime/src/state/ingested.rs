// SPDX-License-Identifier: CC0-1.0

//! What the INGESTION data plane holds for the dataset group a campaign or recommender
//! was fitted to: the half of an inference call's input that keeps changing after the
//! catalog was fitted.
//!
//! # Why the inference plane reads another service's store
//!
//! Amazon Personalize's real-time data plane is coupled to real-time ingestion: an
//! interaction recorded through `PutEvents` / `PutActionInteractions` and a record put
//! through `PutItems` / `PutActions` change what a campaign of the same dataset group
//! returns, without a new import or a new solution version. Both halves run on ONE
//! engine here, so the inference plane reads the ingestion store the Amazon Personalize
//! Events crate owns rather than keeping a copy of it that could go stale.
//!
//! A pure READ of that store: nothing here writes to it, so a recommendation call leaves
//! every ingested record exactly as the call that put it left it.
//!
//! # What a fitted catalog and the live records each contribute
//!
//! A campaign's registered catalog ([`Campaign::items`](super::Campaign::items)) is what
//! its solution version was FITTED to, at the instant it was fitted
//! ([`Campaign::fitted_at`](super::Campaign::fitted_at)). The ingestion store holds
//! everything that reached the group's datasets and trackers, before and after that
//! instant. So the catalog one call recommends from is:
//!
//! * every entry the fitted catalog holds, plus every entry a record in the group's
//!   Items/Actions dataset describes and every entry one of its interactions mentions —
//!   which is what makes an item put after training recommendable at all;
//! * each entry's metadata columns, with the columns of its ingested record applied over
//!   the fitted ones (the latest put for a column is what that column holds);
//! * each entry's popularity: the popularity already counted for the entry plus every
//!   interaction that count does not include. The interactions the fitted catalog already
//!   counted are not counted twice, which is the only reason the fitted instant is kept —
//!   so that instant separates the interactions of an entry the fitted catalog HOLDS a
//!   popularity for, and an entry it holds none for counts every interaction recorded for
//!   it, whenever that interaction arrived.
//!
//! A filter clause over an interaction dataset reads something else entirely — whether
//! THIS user interacted with the entry — so it reads every recorded interaction of that
//! user, whenever it arrived: a user's purchase does not stop being a purchase because a
//! model was fitted after it.
//!
//! # Bounds
//!
//! A dataset group's trackers and datasets each retain a bounded number of records, but a
//! group may hold many trackers, so what ONE call reads is bounded here as well:
//! [`MAX_LIVE_INTERACTIONS`] interaction rows (the most recent ones) and
//! [`MAX_LIVE_CATALOG_ENTRIES`] entries added to a fitted catalog. Both are this
//! emulator's own capacity, not a documented AWS quota.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ::service_com_amazonaws_personalizeevents_amazonpersonalizeevents::state as ingestion;

use super::filter::EntryFacts;
use super::{Action, Item};

/// The interaction column naming the user whose interaction a row is.
///
/// Source: the fields an Amazon Personalize Interactions and Action interactions schema
/// declares — `USER_ID`, `ITEM_ID` / `ACTION_ID` and `TIMESTAMP` are required, and
/// `EVENT_TYPE`, `EVENT_VALUE`, `IMPRESSION` and `RECOMMENDATION_ID` are the reserved
/// ones — spelled the way a schema field and therefore a filter expression spells them
/// (`Interactions.EVENT_TYPE`).
pub const USER_ID_COLUMN: &str = "USER_ID";
/// The interaction column naming the ITEM a row is about.
pub const ITEM_ID_COLUMN: &str = "ITEM_ID";
/// The interaction column naming the ACTION a row is about.
pub const ACTION_ID_COLUMN: &str = "ACTION_ID";
/// The interaction column carrying the instant of the interaction, in the Unix time the
/// ingested record keeps it in.
pub const TIMESTAMP_COLUMN: &str = "TIMESTAMP";
/// The interaction column naming the KIND of interaction.
pub const EVENT_TYPE_COLUMN: &str = "EVENT_TYPE";
/// The interaction column carrying `Event$eventValue`.
pub const EVENT_VALUE_COLUMN: &str = "EVENT_VALUE";
/// The interaction column carrying `Event$impression`, the list of ids that were shown.
/// A list is held as the `|`-separated text a Personalize categorical column holds.
pub const IMPRESSION_COLUMN: &str = "IMPRESSION";
/// The interaction column carrying `Event$recommendationId`.
pub const RECOMMENDATION_ID_COLUMN: &str = "RECOMMENDATION_ID";

/// The columns an interaction row takes from the stored record itself, which a property
/// of the same name therefore cannot reshape.
const DERIVED_COLUMNS: [&str; 7] = [
    USER_ID_COLUMN,
    ITEM_ID_COLUMN,
    ACTION_ID_COLUMN,
    TIMESTAMP_COLUMN,
    EVENT_TYPE_COLUMN,
    EVENT_VALUE_COLUMN,
    IMPRESSION_COLUMN,
];

/// The most interaction rows one inference call reads out of a dataset group's trackers,
/// the most recent ones first to be kept.
pub const MAX_LIVE_INTERACTIONS: usize = 10_000;

/// The most entries ingestion may ADD to a resource's fitted catalog for one call: the
/// most interacted-with ones, so a bounded catalog is the head of the same ordering a
/// full one would report.
pub const MAX_LIVE_CATALOG_ENTRIES: usize = 1_000;

/// The ARN resource type of a dataset group, whose resource field is
/// `dataset-group/<name>`.
const DATASET_GROUP_RESOURCE_TYPE: &str = "dataset-group";

/// The ARN resource type of a dataset, whose resource field is
/// `dataset/<dataset group name>/<DATASET_TYPE>` — a dataset's identity inside its group
/// is its type, which is what ties a dataset ARN to its group's.
const DATASET_RESOURCE_TYPE: &str = "dataset";

/// One recorded interaction, as the popularity term and a filter clause over an
/// interaction dataset read it.
#[derive(Debug, Clone)]
pub struct Interaction {
    /// The user the interaction belongs to, when the call that recorded it named one.
    pub user_id: Option<String>,
    /// The catalog entry the row is about: the item of an item interaction, the action of
    /// an action interaction.
    pub entity_id: String,
    /// When this emulator ingested the row, which is what separates the interactions a
    /// fitted catalog already counted from the ones recorded since.
    pub received_at: SystemTime,
    /// The row's columns, keyed as an interaction schema names its fields.
    pub columns: BTreeMap<String, String>,
}

/// The live ingestion state of ONE dataset group, read once per inference call.
#[derive(Debug, Clone, Default)]
pub struct Ingested {
    /// The records put into the group's Items dataset, by item id, as metadata columns.
    items: BTreeMap<String, BTreeMap<String, String>>,
    /// The same for its Actions dataset, by action id.
    actions: BTreeMap<String, BTreeMap<String, String>>,
    /// The item interactions the group's trackers recorded, oldest first.
    interactions: Vec<Interaction>,
    /// The action interactions the group's trackers recorded, oldest first.
    action_interactions: Vec<Interaction>,
    /// The instant the resolved resource's catalog was fitted to this group's data;
    /// `None` when nothing fitted it, in which case no interaction is already counted.
    fitted_at: Option<SystemTime>,
}

/// The interaction rows ONE user recorded, grouped by the catalog entry each is about.
///
/// Built once per request, because a clause over an interaction dataset asks the same
/// question of every candidate entry: did THIS user interact with it that way?
#[derive(Debug, Clone, Default)]
pub struct RecordedInteractions {
    rows: BTreeMap<String, Vec<BTreeMap<String, String>>>,
}

impl RecordedInteractions {
    /// The interactions this user recorded for `entry_id`, oldest first — empty when the
    /// user never interacted with it.
    pub fn of(&self, entry_id: &str) -> &[BTreeMap<String, String>] {
        self.rows.get(entry_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Everything a filter clause reads about the entry `entry_id`, whose own dataset
    /// columns are `attributes`: the one place the two halves of a filter's input are put
    /// together, so every operation applying a filter reads the same facts.
    pub fn facts<'a>(
        &'a self,
        entry_id: &str,
        attributes: &'a BTreeMap<String, String>,
    ) -> EntryFacts<'a> {
        EntryFacts::new(attributes, self.of(entry_id))
    }
}

impl Ingested {
    /// The Items catalog an inference call recommends from: `stored` as the catalog was
    /// fitted, brought up to date with what the ingestion data plane holds for the group.
    pub fn items(&self, stored: &[Item]) -> Vec<Item> {
        merge(
            stored
                .iter()
                .map(|item| (item.item_id.clone(), item.base_score, item.attributes.clone())),
            &self.items,
            &self.interactions,
            self.fitted_at,
        )
        .into_iter()
        .map(|(item_id, base_score, attributes)| Item {
            item_id,
            base_score,
            attributes,
        })
        .collect()
    }

    /// The same for the Actions catalog, from the group's Actions records and its action
    /// interactions.
    pub fn actions(&self, stored: &[Action]) -> Vec<Action> {
        merge(
            stored.iter().map(|action| {
                (
                    action.action_id.clone(),
                    action.base_score,
                    action.attributes.clone(),
                )
            }),
            &self.actions,
            &self.action_interactions,
            self.fitted_at,
        )
        .into_iter()
        .map(|(action_id, base_score, attributes)| Action {
            action_id,
            base_score,
            attributes,
        })
        .collect()
    }

    /// The item interactions `user_id` recorded, grouped by item. A request that
    /// personalizes for no user (an empty `user_id`) recorded none, so a clause over an
    /// interaction dataset matches nothing for it.
    pub fn interactions_of(&self, user_id: &str) -> RecordedInteractions {
        recorded(&self.interactions, user_id)
    }

    /// The same for action interactions, grouped by action.
    pub fn action_interactions_of(&self, user_id: &str) -> RecordedInteractions {
        recorded(&self.action_interactions, user_id)
    }
}

/// The live ingestion state of `dataset_group_arn`, read out of the ingestion store the
/// same engine carries.
///
/// A resource that was fitted to no dataset group (`None`) reads nothing: there is no
/// group whose records are its data, so the fitted catalog is the whole input. So does an
/// engine that does not carry the ingestion store at all — this is a read, and an absent
/// store holds no records rather than an error.
///
/// LOCK ORDER: the caller holds this service's own registry; this takes the ingestion
/// store second, the same order the Personalize control plane takes the two in.
pub fn live_view<Eng: Send + Sync + 'static>(
    engine: &Eng,
    dataset_group_arn: Option<&str>,
    fitted_at: Option<SystemTime>,
) -> Ingested {
    let Some(dataset_group_arn) = dataset_group_arn else {
        return Ingested::default();
    };
    let Some(store) = ingestion::service_state(engine) else {
        return Ingested::default();
    };
    let guard = store.lock();
    read(&guard, dataset_group_arn, fitted_at)
}

/// [`live_view`] from an ingestion store already in hand.
pub fn read(
    store: &ingestion::ComAmazonawsPersonalizeeventsAmazonpersonalizeeventsState,
    dataset_group_arn: &str,
    fitted_at: Option<SystemTime>,
) -> Ingested {
    let mut live = Ingested {
        fitted_at,
        ..Ingested::default()
    };

    for (arn, dataset) in &store.datasets {
        if !in_dataset_group(arn, dataset_group_arn) {
            continue;
        }
        match dataset.dataset_type {
            Some(ingestion::DatasetType::Items) => {
                collect_records(&mut live.items, dataset, ingestion::MetadataKind::Items);
            }
            Some(ingestion::DatasetType::Actions) => {
                collect_records(&mut live.actions, dataset, ingestion::MetadataKind::Actions);
            }
            // A Users, Interactions or Action interactions dataset holds no entity this
            // service recommends: the two streams are read off the group's trackers below,
            // where the interaction records actually live.
            _ => {}
        }
    }

    for tracker in store
        .event_trackers
        .values()
        .filter(|tracker| tracker.dataset_group_arn.as_deref() == Some(dataset_group_arn))
    {
        for event in &tracker.events {
            // An event that mentions no item is about no catalog entry: it is a session
            // event, and nothing in a recommendation response is a function of it.
            let Some(item_id) = event.item_id.clone() else {
                continue;
            };
            live.interactions.push(Interaction {
                user_id: event.user_id.clone(),
                entity_id: item_id,
                received_at: event.received_at,
                columns: event_columns(event),
            });
        }
        for interaction in &tracker.action_interactions {
            live.action_interactions.push(Interaction {
                user_id: interaction.user_id.clone(),
                entity_id: interaction.action_id.clone(),
                received_at: interaction.received_at,
                columns: action_interaction_columns(interaction),
            });
        }
    }
    retain_most_recent(&mut live.interactions);
    retain_most_recent(&mut live.action_interactions);
    live
}

/// Whether `dataset_arn` names a dataset of the dataset group `group_arn`.
///
/// A Personalize dataset ARN's resource is `dataset/<dataset group name>/<DATASET_TYPE>`
/// and a dataset group's is `dataset-group/<name>`, so a dataset belongs to the group
/// whose name its own resource names, in the same partition, vendor, region and account.
/// Read through this tree's one ARN parse (`service_common::arn`).
fn in_dataset_group(dataset_arn: &str, group_arn: &str) -> bool {
    let Some(group) = ::service_common::arn::parse(group_arn) else {
        return false;
    };
    let Some(dataset) = ::service_common::arn::parse(dataset_arn) else {
        return false;
    };
    if (group.partition, group.vendor, group.region, group.account)
        != (
            dataset.partition,
            dataset.vendor,
            dataset.region,
            dataset.account,
        )
    {
        return false;
    }
    let Some(name) =
        ::service_common::arn::resource_id_of_type(group_arn, DATASET_GROUP_RESOURCE_TYPE)
    else {
        return false;
    };
    ::service_common::arn::resource_id_of_type(dataset_arn, DATASET_RESOURCE_TYPE)
        .and_then(|resource| resource.strip_prefix(name))
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Copy the `kind` records of `dataset` into `into`, as metadata columns keyed by entity
/// id. A record put again for an id replaced the earlier one where it was stored, so the
/// columns here are the ones the latest put left.
fn collect_records(
    into: &mut BTreeMap<String, BTreeMap<String, String>>,
    dataset: &ingestion::Dataset,
    kind: ingestion::MetadataKind,
) {
    for (id, record) in dataset.records(kind) {
        let columns = into.entry(id.clone()).or_default();
        for (name, value) in properties(record.properties.as_ref()) {
            columns.insert(name, value);
        }
    }
}

/// The columns of one ingested item-interaction event.
///
/// `TIMESTAMP` is `Event$sentAt`, "the timestamp (in Unix time) on the client side when
/// the event occurred" — the instant of the interaction itself, which is what an
/// interaction row is keyed by in time rather than when this emulator received it.
fn event_columns(event: &ingestion::StoredEvent) -> BTreeMap<String, String> {
    let mut columns = BTreeMap::new();
    if let Some(user_id) = &event.user_id {
        columns.insert(USER_ID_COLUMN.to_string(), user_id.clone());
    }
    if let Some(item_id) = &event.item_id {
        columns.insert(ITEM_ID_COLUMN.to_string(), item_id.clone());
    }
    columns.insert(
        TIMESTAMP_COLUMN.to_string(),
        event.sent_at.secs().to_string(),
    );
    columns.insert(EVENT_TYPE_COLUMN.to_string(), event.event_type.clone());
    if let Some(event_value) = event.event_value {
        columns.insert(EVENT_VALUE_COLUMN.to_string(), event_value.to_string());
    }
    if !event.impression.is_empty() {
        columns.insert(IMPRESSION_COLUMN.to_string(), event.impression.join("|"));
    }
    if let Some(recommendation_id) = &event.recommendation_id {
        columns.insert(
            RECOMMENDATION_ID_COLUMN.to_string(),
            recommendation_id.clone(),
        );
    }
    merge_properties(&mut columns, event.properties.as_ref());
    columns
}

/// The columns of one ingested action-interaction event, keyed in time by
/// `ActionInteraction$timestamp` for the same reason [`event_columns`] uses `sentAt`.
fn action_interaction_columns(
    interaction: &ingestion::StoredActionInteraction,
) -> BTreeMap<String, String> {
    let mut columns = BTreeMap::new();
    if let Some(user_id) = &interaction.user_id {
        columns.insert(USER_ID_COLUMN.to_string(), user_id.clone());
    }
    columns.insert(ACTION_ID_COLUMN.to_string(), interaction.action_id.clone());
    columns.insert(
        TIMESTAMP_COLUMN.to_string(),
        interaction.timestamp.secs().to_string(),
    );
    columns.insert(
        EVENT_TYPE_COLUMN.to_string(),
        interaction.event_type.clone(),
    );
    if !interaction.impression.is_empty() {
        columns.insert(
            IMPRESSION_COLUMN.to_string(),
            interaction.impression.join("|"),
        );
    }
    if let Some(recommendation_id) = &interaction.recommendation_id {
        columns.insert(
            RECOMMENDATION_ID_COLUMN.to_string(),
            recommendation_id.clone(),
        );
    }
    merge_properties(&mut columns, interaction.properties.as_ref());
    columns
}

/// Merge a record's `properties` into `columns`, DROPPING any whose name is one of the
/// columns the row takes from the record itself ([`DERIVED_COLUMNS`]): what identifies a
/// row and when it happened come from the record, so a property a caller happens to name
/// the same cannot reshape it. Names are compared case-insensitively, the way Personalize
/// compares a schema field name.
fn merge_properties(
    columns: &mut BTreeMap<String, String>,
    raw: Option<&::serde_json::Value>,
) {
    for (name, value) in properties(raw) {
        if DERIVED_COLUMNS
            .iter()
            .any(|column| column.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        columns.insert(name, value);
    }
}

/// The members of a stored `properties` object as columns.
///
/// A property whose value is not a string is rendered as the JSON it was ingested as, so
/// a reader sees the value that was actually put rather than nothing at all.
fn properties(raw: Option<&::serde_json::Value>) -> Vec<(String, String)> {
    let Some(::serde_json::Value::Object(properties)) = raw else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, value)| {
            let rendered = match value {
                ::serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            (name.clone(), rendered)
        })
        .collect()
}

/// Keep the most recent [`MAX_LIVE_INTERACTIONS`] rows: a group may hold many trackers,
/// so what one call reads is bounded here even though each tracker's own stream is.
fn retain_most_recent(rows: &mut Vec<Interaction>) {
    if rows.len() <= MAX_LIVE_INTERACTIONS {
        return;
    }
    rows.sort_by_key(|row| row.received_at);
    let excess = rows.len() - MAX_LIVE_INTERACTIONS;
    rows.drain(..excess);
}

/// The rows of `user_id`, grouped by the entry each is about. An empty `user_id`
/// identifies no user, so it recorded nothing.
fn recorded(rows: &[Interaction], user_id: &str) -> RecordedInteractions {
    let mut recorded = RecordedInteractions::default();
    if user_id.is_empty() {
        return recorded;
    }
    for row in rows
        .iter()
        .filter(|row| row.user_id.as_deref() == Some(user_id))
    {
        recorded
            .rows
            .entry(row.entity_id.clone())
            .or_default()
            .push(row.columns.clone());
    }
    recorded
}

/// The catalog one call recommends from, as `(id, popularity, columns)` triples: the one
/// rule both the Items and the Actions catalog follow.
///
/// `stored` is the fitted catalog in its stored order, which is kept: the entries
/// ingestion ADDS are appended, most interacted-with first and bounded by
/// [`MAX_LIVE_CATALOG_ENTRIES`], and every operation orders the result by the score it
/// computes anyway.
///
/// An entry's popularity is what is already counted for it plus every interaction that
/// count does not include: for an entry `stored` holds, its stored popularity plus the
/// rows recorded after `fitted_at`; for an entry `stored` holds none for, every recorded
/// row, since no popularity counted any of them.
fn merge(
    stored: impl Iterator<Item = (String, f64, BTreeMap<String, String>)>,
    records: &BTreeMap<String, BTreeMap<String, String>>,
    rows: &[Interaction],
    fitted_at: Option<SystemTime>,
) -> Vec<(String, f64, BTreeMap<String, String>)> {
    // Every interaction recorded for an entry, and the ones recorded since the catalog was
    // fitted. An entry the fitted catalog holds a popularity for counts only the latter:
    // the earlier ones are part of that stored popularity, so counting them again would
    // weigh one interaction twice. An entry it holds no popularity for counts all of them,
    // because none of them was ever counted.
    let mut recorded_rows: BTreeMap<&str, usize> = BTreeMap::new();
    let mut since_fitted: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows {
        *recorded_rows.entry(row.entity_id.as_str()).or_default() += 1;
        if fitted_at.is_none_or(|fitted_at| row.received_at > fitted_at) {
            *since_fitted.entry(row.entity_id.as_str()).or_default() += 1;
        }
    }

    let mut merged: Vec<(String, f64, BTreeMap<String, String>)> = Vec::new();
    let mut held: BTreeSet<String> = BTreeSet::new();
    for (id, base_score, mut attributes) in stored {
        if let Some(columns) = records.get(&id) {
            for (name, value) in columns {
                attributes.insert(name.clone(), value.clone());
            }
        }
        let recorded = since_fitted.get(id.as_str()).copied().unwrap_or_default();
        held.insert(id.clone());
        merged.push((id, base_score + recorded as f64, attributes));
    }

    // Entries the fitted catalog does not hold: one a record describes (an item put after
    // training is recommendable), and one only an interaction mentions (as the training
    // pass itself counts an interaction's item an entry).
    let mut added: Vec<(&str, usize)> = records
        .keys()
        .map(String::as_str)
        .chain(rows.iter().map(|row| row.entity_id.as_str()))
        .filter(|id| !held.contains(*id))
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .map(|id| (id, recorded_rows.get(id).copied().unwrap_or_default()))
        .collect();
    // Ordered by that same popularity, so the entries the bound keeps are the most
    // interacted-with ones and a bounded catalog is the head of the ordering a full one
    // would report; the id settles a tie, so the bound always cuts at the same place.
    added.sort_by(|(left_id, left), (right_id, right)| {
        right.cmp(left).then_with(|| left_id.cmp(right_id))
    });
    for (id, recorded) in added.into_iter().take(MAX_LIVE_CATALOG_ENTRIES) {
        merged.push((
            id.to_string(),
            recorded as f64,
            records.get(id).cloned().unwrap_or_default(),
        ));
    }
    merged
}
