// SPDX-License-Identifier: CC0-1.0

//! The capacity figures a table is SERVING, kept for the description an asynchronous
//! capacity move answers with.
//!
//! "UpdateTable is an asynchronous operation; while it's executing, the table status
//! changes from ACTIVE to UPDATING ... Use DescribeTable to monitor the table's status"
//! (the `UpdateTable` doc), and the `TableDescription` it answers with describes the
//! table — so the capacity figures that description carries are the ones in force at the
//! instant the call returns, not the ones the request asked for: the recorded answer to a
//! capacity move reports the pre-move `ReadCapacityUnits`/`WriteCapacityUnits`, the
//! pre-move warm pair and a `NumberOfDecreasesToday` that does not yet count the decrease
//! just accepted. What the move leaves in force is what the reads after it report.
//!
//! A move of the capacity MODE is not of that kind for the PROVISIONED pair: the mode a
//! request resolves to is in force from the moment the call returns — its `BillingModeSummary`
//! and `TableThroughputModeSummary` report it — and the provisioned pair belongs to the mode,
//! an on-demand table provisioning no capacity at all, so a description whose mode has moved
//! carries the new mode's own pair. What the table keeps WARM is a setting of its own rather
//! than a figure of the mode, so that half is still the generation in force
//! ([`CapacityInForce::restate_warm`]). The caller of this type is what decides that (see
//! `UpdateTable`); what this type holds is one generation of one table's figures.
//!
//! The two DATE members beside those figures are not of that generation: "the date and
//! time of the last provisioned throughput increase" / "decrease for this table"
//! (`ProvisionedThroughputDescription$LastIncreaseDateTime` /
//! `$LastDecreaseDateTime`) is stamped when the move is accepted, which the recorded
//! answer to a capacity move already carries, so each stays as the accepted update wrote
//! it.
//!
//! A file of its own beside the table: this is about WHICH GENERATION of one table's
//! capacity figures a description carries, not about what the store holds. It is
//! re-exported up to `crate::state`, so every caller names it `crate::state::…`.

use std::collections::HashMap;

use super::*;

/// The capacity figures one resource — a table, or one global secondary index of it — was
/// serving: the provisioned pair, how many of the day's decreases had been applied, and
/// the warm pair.
#[derive(Debug, Clone, Default)]
struct Figures {
    provisioned: Option<(Option<i64>, Option<i64>)>,
    decreases_today: Option<i64>,
    warm: Option<(Option<i64>, Option<i64>)>,
}

impl Figures {
    /// Report these figures through `provisioned` and `warm`, the members of the
    /// description taken once the move was applied.
    ///
    /// Only the FIGURES are put back. The status beside the warm pair is the move's own —
    /// a pair a change is in flight for is `UPDATING` while it still reports the pair in
    /// force — and so are the two date members beside the provisioned pair, each stamped
    /// when the move was accepted.
    fn restate(
        &self,
        provisioned: Option<&mut ProvisionedThroughputDescription>,
        warm_read: Option<&mut Option<i64>>,
        warm_write: Option<&mut Option<i64>>,
    ) {
        if let Some(description) = provisioned {
            if let Some((read, write)) = self.provisioned {
                description.read_capacity_units = read;
                description.write_capacity_units = write;
            }
            if self.decreases_today.is_some() {
                description.number_of_decreases_today = self.decreases_today;
            }
        }
        self.restate_warm(warm_read, warm_write);
    }

    /// Report the WARM pair these figures carry, and leave the provisioned pair beside it
    /// alone.
    ///
    /// What the table keeps warm is a setting of its own — "the minimum provisioned capacity
    /// maintained for immediate data access" — rather than a figure of the capacity mode, so
    /// a move that changes the MODE still answers with the warm pair in force while it
    /// settles.
    fn restate_warm(
        &self,
        warm_read: Option<&mut Option<i64>>,
        warm_write: Option<&mut Option<i64>>,
    ) {
        if let Some((read, write)) = self.warm {
            if let Some(slot) = warm_read {
                *slot = read;
            }
            if let Some(slot) = warm_write {
                *slot = write;
            }
        }
    }
}

/// The capacity figures one table was serving before a capacity move was applied to it:
/// the table's own, and each global secondary index's by index name.
///
/// Taken from the `TableDescription` of the table as it stood, so there is one reading of
/// what a table reports ([`Table::describe`]) rather than a second one here.
#[derive(Debug, Clone, Default)]
pub struct CapacityInForce {
    table: Figures,
    indexes: HashMap<String, Figures>,
}

impl CapacityInForce {
    /// What `description` — the table as it stood before the move — reports as the
    /// capacity in force.
    pub fn of(description: &TableDescription) -> Self {
        Self {
            table: Figures {
                provisioned: description.provisioned_throughput.as_ref().map(|throughput| {
                    (
                        throughput.read_capacity_units,
                        throughput.write_capacity_units,
                    )
                }),
                decreases_today: description
                    .provisioned_throughput
                    .as_ref()
                    .and_then(|throughput| throughput.number_of_decreases_today),
                warm: description
                    .warm_throughput
                    .as_ref()
                    .map(|warm| (warm.read_units_per_second, warm.write_units_per_second)),
            },
            indexes: description
                .global_secondary_indexes
                .iter()
                .flatten()
                .filter_map(|index| {
                    let name = index.index_name.clone()?;
                    let figures = Figures {
                        provisioned: index.provisioned_throughput.as_ref().map(|throughput| {
                            (
                                throughput.read_capacity_units,
                                throughput.write_capacity_units,
                            )
                        }),
                        decreases_today: index
                            .provisioned_throughput
                            .as_ref()
                            .and_then(|throughput| throughput.number_of_decreases_today),
                        warm: index
                            .warm_throughput
                            .as_ref()
                            .map(|warm| (warm.read_units_per_second, warm.write_units_per_second)),
                    };
                    Some((name, figures))
                })
                .collect(),
        }
    }

    /// Report that capacity in `description`, the one taken once the move was applied.
    ///
    /// Every member that is not a capacity figure is what the accepted update leaves: the
    /// table's status, its capacity mode, and the indexes this call added or removed. An
    /// index this call CREATED has no earlier generation, so it keeps the figures it was
    /// created with.
    pub fn restate(&self, description: &mut TableDescription) {
        self.put_back(description, true);
    }

    /// Report only the WARM pair of that capacity in `description`, for a call whose
    /// PROVISIONED pair belongs to a capacity mode the update moved the table into (see this
    /// module's docs and `UpdateTable`).
    pub fn restate_warm(&self, description: &mut TableDescription) {
        self.put_back(description, false);
    }

    /// The one walk over a description both of those take, reporting the provisioned pair as
    /// well only when `provisioned` says to.
    fn put_back(&self, description: &mut TableDescription, provisioned: bool) {
        let (warm_read, warm_write) = match description.warm_throughput.as_mut() {
            Some(warm) => (
                Some(&mut warm.read_units_per_second),
                Some(&mut warm.write_units_per_second),
            ),
            None => (None, None),
        };
        if provisioned {
            self.table.restate(
                description.provisioned_throughput.as_mut(),
                warm_read,
                warm_write,
            );
        } else {
            self.table.restate_warm(warm_read, warm_write);
        }
        for index in description.global_secondary_indexes.iter_mut().flatten() {
            let Some(figures) = index
                .index_name
                .as_ref()
                .and_then(|name| self.indexes.get(name))
            else {
                continue;
            };
            let (warm_read, warm_write) = match index.warm_throughput.as_mut() {
                Some(warm) => (
                    Some(&mut warm.read_units_per_second),
                    Some(&mut warm.write_units_per_second),
                ),
                None => (None, None),
            };
            if provisioned {
                figures.restate(index.provisioned_throughput.as_mut(), warm_read, warm_write);
            } else {
                figures.restate_warm(warm_read, warm_write);
            }
        }
    }
}
