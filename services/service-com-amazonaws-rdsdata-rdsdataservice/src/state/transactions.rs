// SPDX-License-Identifier: CC0-1.0

//! ONE TRANSACTION of a cluster: the copy of a database it writes into, the rows of that
//! database it holds while it runs, and the ways it ends.
//!
//! What a transaction IS here is stated beside the state it belongs to (`super::Transaction`,
//! and the module docs of `super` for the isolation it reads under); this is its behaviour:
//! opening it takes the copy its statements write into, each statement of it is recorded so a
//! later rebase can replay it onto what is committed now, and a commit, a rollback or the
//! clock hands that copy back to the cluster to install or to drop.

use super::*;

impl Transaction {
    /// Open a transaction on `cluster`'s `database`, minting its id and taking its
    /// snapshot. The id is opaque, unique and BOUND to the resource it was opened on
    /// ([`mint_transaction_id`]), well inside the modelled 192-character limit. A cluster
    /// holds at most [`MAX_OPEN_TRANSACTIONS`] open transactions — each is a whole copy of
    /// a database — past which opening another is refused. A transaction that ENDED holds
    /// no database, so it is not one of them.
    pub fn open(
        cluster: &mut Cluster,
        resource_arn: &str,
        secret_arn: &str,
        database: Option<&String>,
        schema: Option<&String>,
    ) -> Result<String, SqlError> {
        if cluster.open_transaction_count() >= MAX_OPEN_TRANSACTIONS {
            return Err(SqlError::Capacity(format!(
                "this emulator holds at most {MAX_OPEN_TRANSACTIONS} open transactions per cluster"
            )));
        }
        let committed = cluster.database_mut(&database_key(database))?;
        let based_on = committed.version();
        let pending = committed.snapshot()?;
        let id = mint_transaction_id(resource_arn);
        let now = SystemTime::now();
        // Who this transaction is in the cluster's lock table, where every surface of the
        // cluster holds the rows its own transactions have written.
        let holder = cluster
            .volume
            .as_ref()
            .map_or(0, |volume| volume.locks().mint_holder());
        cluster.transactions.insert(
            id.clone(),
            Self {
                id: id.clone(),
                resource_arn: resource_arn.to_string(),
                secret_arn: secret_arn.to_string(),
                database: database.cloned(),
                schema: schema.cloned(),
                created_at: now,
                last_used_at: now,
                pending: Some(pending),
                writes: Vec::new(),
                based_on,
                aborted: false,
                ended: None,
                holder,
            },
        );
        Ok(id)
    }

    /// Record that a call used this transaction, restarting its idle timeout.
    pub fn touch(&mut self) {
        self.last_used_at = SystemTime::now();
    }

    /// Whether this transaction has ENDED — a commit installed its writes, a rollback
    /// discarded them, or the clock rolled it back.
    pub fn is_terminal(&self) -> bool {
        self.ended.is_some()
    }

    /// Why this transaction is no longer running, and `None` while it still is.
    pub fn end_cause(&self) -> Option<TransactionEnd> {
        self.ended.map(|ended| ended.cause)
    }

    /// When this transaction stopped running, and `None` while it still is.
    pub(super) fn ended_at(&self) -> Option<SystemTime> {
        self.ended.map(|ended| ended.at)
    }

    /// Whether a statement of this transaction FAILED, so the database ignores every
    /// further command of it until the transaction block ends.
    pub fn is_aborted(&self) -> bool {
        self.aborted
    }

    /// The database this transaction's statements write into.
    ///
    /// `Err` once a statement of this transaction has failed: the database "ignores
    /// commands until end of transaction block", which is an error the DATABASE raises
    /// for every further statement rather than the transaction being gone.
    pub fn open_database(&mut self) -> Result<&mut Database, SqlError> {
        if self.aborted {
            return Err(SqlError::Engine(postgres::transaction_aborted()));
        }
        self.pending
            .as_mut()
            .ok_or(SqlError::TransactionNotFound(TransactionEnd::Terminated))
    }

    /// Whether this transaction's copy was built from the committed database as it is
    /// at `version`, so its next statement already reads everything committed.
    pub(super) fn reads_version(&self, version: u64) -> bool {
        self.based_on == version
    }

    /// Rebuild the copy this transaction runs against from the COMMITTED database as it
    /// is now, with this transaction's own writes replayed onto it.
    ///
    /// What READ COMMITTED means here: a statement of this transaction reads every row
    /// another transaction committed before it ran, alongside the rows this transaction
    /// has written itself and nobody else can see yet. The rows another writer committed
    /// come from the fresh copy and this transaction's own from the replay, so both are
    /// there.
    ///
    /// What the replay does NOT reproduce: a statement that read the database as it was
    /// (`UPDATE t SET n = n + 1`, `INSERT … SELECT`) is applied again to the newer rows,
    /// and one that read a clock or a random value gets another reading. A replayed
    /// statement the newer rows REFUSE (a key another transaction has since committed)
    /// fails as that statement's own failure, which terminates this transaction the way
    /// any failed statement in it does.
    pub(super) fn rebase_onto(
        &mut self,
        committed: &Database,
        budget: &mut RequestBudget,
    ) -> Result<(), SqlError> {
        let mut rebuilt = committed.snapshot()?;
        for write in &self.writes {
            rebuilt.run_within(budget, &write.sql, &write.parameters)?;
        }
        self.pending = Some(rebuilt);
        self.based_on = committed.version();
        Ok(())
    }

    /// Remember a statement of this transaction that CHANGED its database, so a later
    /// rebase puts it back.
    pub(super) fn record(&mut self, sql: &str, parameters: &[SqlParameter], outcome: &StatementOutcome) {
        if !outcome.changed_it() {
            return;
        }
        self.writes.push(RecordedWrite {
            sql: sql.to_string(),
            parameters: parameters.to_vec(),
        });
    }

    /// Refuse a transaction that is already holding [`MAX_TRANSACTION_WRITES`]
    /// statements one more, BEFORE it runs: the statements a transaction has written are
    /// kept until it ends (that is what a rebase replays and a commit installs), so their
    /// number is a store one caller can grow and is capped like every other store here.
    pub(super) fn require_room_for(&self, statements: usize) -> Result<(), SqlError> {
        if self.writes.len() + statements > MAX_TRANSACTION_WRITES {
            return Err(SqlError::Capacity(format!(
                "this emulator holds at most {MAX_TRANSACTION_WRITES} statements per open \
                 transaction"
            )));
        }
        Ok(())
    }

    /// End this transaction and hand back the database it was writing into, so the caller
    /// can install it (a commit) or read the sequence marks out of it and drop it (a
    /// rollback). The id stays in the cluster, now ended.
    ///
    /// `Err` for one that has already ended, which is what refuses a second commit or
    /// rollback of the same id.
    pub fn end(&mut self) -> Result<Database, SqlError> {
        let pending = self.pending.take().ok_or_else(|| {
            SqlError::TransactionNotFound(self.end_cause().unwrap_or(TransactionEnd::Terminated))
        })?;
        self.ended = Some(Ended {
            at: SystemTime::now(),
            cause: TransactionEnd::Terminated,
        });
        Ok(pending)
    }

    /// Hand back the copy this transaction has been writing into, for a caller that
    /// installs it as the committed database while the transaction GOES ON — the
    /// implicit commit a statement of a class the engine does not run inside a
    /// transaction earns ([`Cluster::commits_the_transaction`]).
    ///
    /// Unlike [`Self::end`] this does not end the transaction: it is left reading a fresh
    /// copy of the database that is about to be committed, with the statements it had
    /// written forgotten — they are committed now, so nothing can replay them and nothing
    /// is left for a rollback to discard. The copy it carries on with is taken BEFORE the
    /// installed one is handed over, so a transaction whose copy could not be taken keeps
    /// the one it had.
    pub(super) fn commit_in_place(&mut self) -> Result<Database, SqlError> {
        let pending = self.pending.as_ref().ok_or_else(|| {
            SqlError::TransactionNotFound(self.end_cause().unwrap_or(TransactionEnd::Terminated))
        })?;
        let carried_on = pending.snapshot()?;
        self.based_on = carried_on.version();
        let installed = self
            .pending
            .replace(carried_on)
            .expect("the copy was just read");
        self.writes.clear();
        Ok(installed)
    }

    /// Record that a statement of this transaction FAILED, which is what terminates it:
    /// "a transaction is terminated when a statement in it fails"
    /// (`BeginTransaction`'s own documentation). Nothing it wrote can reach the committed
    /// database any more — a commit of it discards its writes exactly as a rollback does —
    /// and the copy it was writing into is kept until one of them ends it, because that is
    /// what they discard.
    pub fn abort(&mut self) {
        self.aborted = true;
    }

    /// End this transaction if the CLOCK has already ended it, and hand back the copy of
    /// the database it was writing into so its sequence marks can be carried over before
    /// it is dropped. `None` for one the clock does not owe a rollback, and for one that
    /// has already stopped running.
    ///
    /// Two windows end a transaction nobody ended:
    ///
    /// * it sat IDLE for [`TRANSACTION_IDLE_TIMEOUT`] — "a transaction times out if no
    ///   calls use its transaction ID in three minutes", and a transaction that timed out
    ///   "is rolled back automatically" — so its id is answered from then on as one the
    ///   service abandoned for inactivity ([`TransactionEnd::Idle`]);
    /// * it has been open for [`TRANSACTION_MAX_LIFETIME`] — "a transaction is terminated
    ///   and rolled back automatically after 24 hours" — which the service words as a
    ///   termination like any other ([`TransactionEnd::Terminated`]).
    ///
    /// Each is dated at the instant it FELL DUE and not at the call that noticed, so how
    /// long a terminal record is then kept does not depend on when anybody looked.
    pub(super) fn end_if_the_clock_has(&mut self) -> Option<Database> {
        if self.is_terminal() {
            return None;
        }
        let idle_since = self.last_used_at + TRANSACTION_IDLE_TIMEOUT;
        let lifetime_since = self.created_at + TRANSACTION_MAX_LIFETIME;
        let now = SystemTime::now();
        let (at, cause) = if idle_since <= now {
            (idle_since, TransactionEnd::Idle)
        } else if lifetime_since <= now {
            (lifetime_since, TransactionEnd::Terminated)
        } else {
            return None;
        };
        let pending = self.pending.take();
        // The statements it wrote go with the copy they wrote into: nothing can replay
        // them any more.
        self.writes.clear();
        self.ended = Some(Ended { at, cause });
        pending
    }

    /// Whether this transaction can be FORGOTTEN: an ended one past the window a terminal
    /// id is kept for. Its id is then answered as one of a transaction that ended all the
    /// same, which is what a forgotten record and a remembered one have in common.
    pub(super) fn is_forgotten(&self) -> bool {
        self.ended_at().is_some_and(|at| {
            at.elapsed()
                .is_ok_and(|since| since >= TERMINAL_TRANSACTION_MEMORY)
        })
    }
}
