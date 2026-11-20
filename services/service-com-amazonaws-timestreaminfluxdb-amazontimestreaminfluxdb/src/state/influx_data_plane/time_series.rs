// SPDX-License-Identifier: CC0-1.0

//! The in-process time-series store ONE DB instance's InfluxDB v2 data plane reads and
//! writes, and the admin credential that plane authenticates against.
//!
//! A DB instance is created with an initial admin user, an initial organization and an
//! initial bucket (`CreateDbInstance`'s `username`, `organization` and `bucket`), and
//! `/api/v2/write` carries the points a client sends as InfluxDB line protocol. Both live
//! here: the control-plane handler builds one of these as it stores the record, and the
//! socket the instance is published on
//! ([`super::serve_db_instance_plane`]) is handed a handle on it — so every point a write
//! stored is the point a later read of that instance sees, and nothing a client sends is
//! answered from anywhere but this store.
//!
//! The password is NOT held: only its SHA-256 digest is, which is all an authentication
//! decision needs. Nothing here is written to a file, and nothing leaves the process.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// A handle on one DB instance's data: cheap to clone, so the socket's per-connection
/// handler owns one and every connection reads the same store the control plane wrote.
pub type SharedTimeSeries = Arc<Mutex<TimeSeries>>;

/// How many points ONE bucket of this store holds.
///
/// NOT a claim about any InfluxDB retention policy: the emulator's OWN bound on the data a
/// client can make it hold, so a writer looping forever cannot grow the process without
/// end. Past it the oldest points of that bucket are dropped, which is the shape a bucket's
/// retention period has — the newest points are the ones a query answers from.
pub const MAX_POINTS_PER_BUCKET: usize = 20_000;

/// How many sign-in sessions one instance's plane holds at once; past it the oldest is
/// forgotten. The emulator's OWN bound, for the same reason as [`MAX_POINTS_PER_BUCKET`].
pub const MAX_SESSIONS: usize = 64;

/// One point a client wrote: the series it belongs to, its fields, and when it was taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    /// The measurement the series belongs to.
    pub measurement: String,
    /// The tag set, in key order — the rest of the series key.
    pub tags: BTreeMap<String, String>,
    /// The fields the point carries, in the order the line protocol wrote them.
    pub fields: Vec<(String, FieldValue)>,
    /// The point's timestamp in nanoseconds since the Unix epoch.
    pub timestamp_ns: i64,
}

/// A field value, in the five types InfluxDB line protocol admits.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// A float field.
    Float(f64),
    /// A signed integer field (`1i`).
    Integer(i64),
    /// An unsigned integer field (`1u`).
    Unsigned(u64),
    /// A boolean field.
    Boolean(bool),
    /// A string field.
    Text(String),
}

impl FieldValue {
    /// The value as an annotated-CSV cell renders it.
    #[must_use]
    pub fn rendered(&self) -> String {
        match self {
            Self::Float(value) => format!("{value}"),
            Self::Integer(value) => format!("{value}"),
            Self::Unsigned(value) => format!("{value}"),
            Self::Boolean(value) => format!("{value}"),
            Self::Text(value) => value.clone(),
        }
    }
}

/// The initial admin user an instance's data plane authenticates, as the control plane
/// stored it: the user name, a SALTED SHA-256 digest of the password (never the password),
/// and the organization and bucket the instance was created with.
#[derive(Debug, Clone)]
pub struct AdminCredential {
    /// `CreateDbInstance`'s `username`, or InfluxDB's own default when the request named
    /// none.
    pub username: String,
    /// The salt this credential's digest was taken over, drawn once per credential.
    salt: String,
    /// `hex(sha256(salt || password))`. The password itself is not held anywhere, and the
    /// salt is what keeps the digest specific to this one credential.
    password_digest: String,
    /// `CreateDbInstance`'s `organization`, or the default below when the request named
    /// none.
    pub organization: String,
    /// `CreateDbInstance`'s `bucket`, or the default below when the request named none.
    pub bucket: String,
}

/// The admin user name an instance takes when the create named none. InfluxDB v2's own
/// initial-setup default user.
pub const DEFAULT_USERNAME: &str = "admin";

/// The organization an instance takes when the create named none.
pub const DEFAULT_ORGANIZATION: &str = "default";

/// The bucket an instance takes when the create named none.
pub const DEFAULT_BUCKET: &str = "default";

impl AdminCredential {
    /// The credential of an instance created with these values, with the password reduced
    /// to a salted digest here and nowhere else.
    #[must_use]
    pub fn new(
        username: Option<&str>,
        password: &str,
        organization: Option<&str>,
        bucket: Option<&str>,
    ) -> Self {
        let salt = drawn_salt();
        Self {
            username: username.unwrap_or(DEFAULT_USERNAME).to_string(),
            password_digest: password_digest(&salt, password),
            salt,
            organization: organization.unwrap_or(DEFAULT_ORGANIZATION).to_string(),
            bucket: bucket.unwrap_or(DEFAULT_BUCKET).to_string(),
        }
    }

    /// Whether `username` and `password` are this instance's admin user.
    ///
    /// The password is compared as its salted digest, so the plaintext is held nowhere; the
    /// comparison is over the whole fixed-length hexadecimal rendering.
    #[must_use]
    pub fn authenticates(&self, username: &str, password: &str) -> bool {
        username == self.username
            && password_digest(&self.salt, password) == self.password_digest
    }
}

/// A salt drawn for one credential.
///
/// The bits come from the standard library's OS-seeded
/// [`std::collections::hash_map::RandomState`] — a fresh one draws its own keys — so two
/// credentials over the same password do not share a digest and a digest says nothing about
/// any other instance's.
fn drawn_salt() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;

    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&RandomState::new().hash_one(0_u8).to_le_bytes());
    bytes[8..].copy_from_slice(&RandomState::new().hash_one(u64::MAX).to_le_bytes());
    hex::encode(bytes)
}

/// `hex(sha256(salt || password))` — the one place a password is reduced for storage and
/// for comparison, so the two cannot differ.
fn password_digest(salt: &str, password: &str) -> String {
    let mut digest = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut digest, salt.as_bytes());
    sha2::Digest::update(&mut digest, password.as_bytes());
    hex::encode(sha2::Digest::finalize(digest))
}

/// One DB instance's data: its admin credential, its buckets' points, and the sign-in
/// sessions its plane has minted.
#[derive(Debug)]
pub struct TimeSeries {
    /// The admin user this instance's plane authenticates.
    pub credential: AdminCredential,
    /// Points by bucket name, oldest first.
    buckets: BTreeMap<String, Vec<Point>>,
    /// The session ids `/api/v2/signin` minted, in mint order — which is what lets the
    /// oldest be forgotten at [`MAX_SESSIONS`].
    sessions: Vec<String>,
    /// The same ids for lookup.
    session_set: BTreeSet<String>,
    /// Monotonic counter mixed into every minted session id.
    session_seq: u64,
}

impl TimeSeries {
    /// The store of an instance created with `credential`: the initial bucket exists and
    /// holds no point, which is what InfluxDB's own initial setup leaves behind.
    #[must_use]
    pub fn new(credential: AdminCredential) -> Self {
        let mut buckets = BTreeMap::new();
        buckets.insert(credential.bucket.clone(), Vec::new());
        Self {
            credential,
            buckets,
            sessions: Vec::new(),
            session_set: BTreeSet::new(),
            session_seq: 0,
        }
    }

    /// Whether `organization` is this instance's organization.
    #[must_use]
    pub fn has_organization(&self, organization: &str) -> bool {
        organization == self.credential.organization
    }

    /// Whether `bucket` is a bucket of this instance.
    #[must_use]
    pub fn has_bucket(&self, bucket: &str) -> bool {
        self.buckets.contains_key(bucket)
    }

    /// Append `points` to `bucket`, dropping the oldest past [`MAX_POINTS_PER_BUCKET`].
    ///
    /// `false`, storing nothing, for a bucket this instance does not have: a bucket exists
    /// only because the instance was created with it, so a write never mints one — which is
    /// both what InfluxDB answers for an unknown bucket and what keeps the whole store
    /// bounded by [`MAX_POINTS_PER_BUCKET`] times the buckets the instance HAS, rather than
    /// by how many names a client can invent.
    pub fn write(&mut self, bucket: &str, points: Vec<Point>) -> bool {
        let Some(held) = self.buckets.get_mut(bucket) else {
            return false;
        };
        held.extend(points);
        if held.len() > MAX_POINTS_PER_BUCKET {
            let overflow = held.len() - MAX_POINTS_PER_BUCKET;
            held.drain(..overflow);
        }
        true
    }

    /// The points `bucket` holds, oldest first, or an empty slice for a bucket this
    /// instance does not have.
    #[must_use]
    pub fn points(&self, bucket: &str) -> &[Point] {
        self.buckets.get(bucket).map_or(&[], Vec::as_slice)
    }

    /// Mint a sign-in session and report its id.
    ///
    /// The bits come from the standard library's OS-seeded
    /// [`std::collections::hash_map::RandomState`] — a fresh one draws its own keys, so one
    /// id does not reveal the next — mixed with the monotonic counter and the wall clock,
    /// then reduced to hexadecimal. Nothing about the user or the password goes into it.
    pub fn open_session(&mut self) -> String {
        use std::collections::hash_map::RandomState;
        use std::hash::BuildHasher;

        self.session_seq += 1;
        let nanos = ::std::time::SystemTime::now()
            .duration_since(::std::time::SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let mut digest = <sha2::Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut digest, self.session_seq.to_le_bytes());
        sha2::Digest::update(&mut digest, nanos.to_le_bytes());
        sha2::Digest::update(
            &mut digest,
            RandomState::new().hash_one(self.session_seq).to_le_bytes(),
        );
        sha2::Digest::update(
            &mut digest,
            RandomState::new().hash_one(!self.session_seq).to_le_bytes(),
        );
        let id = hex::encode(sha2::Digest::finalize(digest));
        self.sessions.push(id.clone());
        self.session_set.insert(id.clone());
        while self.sessions.len() > MAX_SESSIONS {
            let oldest = self.sessions.remove(0);
            self.session_set.remove(&oldest);
        }
        id
    }

    /// Whether `session` is a session this plane minted and still holds.
    #[must_use]
    pub fn has_session(&self, session: &str) -> bool {
        self.session_set.contains(session)
    }

    /// A copy of this instance's data as it stands at this instant — what a backup of the
    /// instance captures.
    #[must_use]
    pub fn snapshot(&self) -> TimeSeriesSnapshot {
        TimeSeriesSnapshot {
            credential: self.credential.clone(),
            buckets: self.buckets.clone(),
        }
    }

    /// The store a resource restored from a backup serves: the admin user and the buckets
    /// the backup captured, and no session at all.
    ///
    /// A session is a live sign-in on the plane the backup was taken from, not data the
    /// backup holds, so the restored plane starts with none and a client signs in again.
    #[must_use]
    pub fn restored(snapshot: TimeSeriesSnapshot) -> Self {
        Self {
            credential: snapshot.credential,
            buckets: snapshot.buckets,
            sessions: Vec::new(),
            session_set: BTreeSet::new(),
            session_seq: 0,
        }
    }
}

/// One DB instance's data as it stood at an instant: the admin user its plane
/// authenticates and the points its buckets held.
///
/// Taken when a backup of the instance is created and kept on that backup's record, so
/// restoring the backup brings back both — the admin user included, which is why a
/// restore request carries no password of its own.
#[derive(Debug, Clone)]
pub struct TimeSeriesSnapshot {
    credential: AdminCredential,
    buckets: BTreeMap<String, Vec<Point>>,
}

impl TimeSeriesSnapshot {
    /// The admin user the captured plane authenticated, and so the one a plane restored
    /// from this snapshot authenticates.
    #[must_use]
    pub fn credential(&self) -> &AdminCredential {
        &self.credential
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store whose admin user is the one every case below authenticates as.
    fn store() -> TimeSeries {
        TimeSeries::new(AdminCredential::new(
            Some("admin"),
            "supersecret1",
            Some("sim2real"),
            Some("metrics"),
        ))
    }

    /// RULE: the admin user authenticates with its own name and password and with nothing
    /// else, and the password is held only as its digest.
    #[test]
    fn only_the_admin_user_and_its_own_password_authenticate() {
        let store = store();
        for (case, username, password, expected) in [
            ("the admin user's own credential", "admin", "supersecret1", true),
            ("the right user, the wrong password", "admin", "supersecret2", false),
            ("the right password, another user", "someone", "supersecret1", false),
            ("neither", "someone", "nothing", false),
            ("an empty password", "admin", "", false),
        ] {
            assert_eq!(
                store.credential.authenticates(username, password),
                expected,
                "case {case:?}: expected {expected}"
            );
        }
        // Neither field the credential keeps holds the password itself.
        assert!(
            !store.credential.salt.contains("supersecret1")
                && !store.credential.password_digest.contains("supersecret1"),
            "case \"the stored credential\": the password itself must not be held"
        );
        // And two credentials over the SAME password are not the same digest, because each
        // draws its own salt.
        let other = TimeSeries::new(AdminCredential::new(
            Some("admin"),
            "supersecret1",
            Some("sim2real"),
            Some("metrics"),
        ));
        assert!(
            other.credential.authenticates("admin", "supersecret1"),
            "case \"a second credential over the same password\": it must authenticate"
        );
    }

    /// RULE: a bucket holds the points written to it, oldest first, and never more than
    /// the store's own bound — past which the OLDEST are the ones dropped.
    #[test]
    fn a_bucket_keeps_the_newest_points_up_to_the_bound() {
        /// One point at `second` carrying `second` as its only field.
        fn point(second: i64) -> Point {
            Point {
                measurement: "cpu".to_string(),
                tags: BTreeMap::new(),
                fields: vec![("value".to_string(), FieldValue::Integer(second))],
                timestamp_ns: second * 1_000_000_000,
            }
        }

        for (case, written) in [
            ("one point", 1usize),
            ("several points", 5),
            ("exactly the bound", MAX_POINTS_PER_BUCKET),
            ("one past the bound", MAX_POINTS_PER_BUCKET + 1),
            ("well past the bound", MAX_POINTS_PER_BUCKET + 97),
        ] {
            let mut store = store();
            for second in 0..written {
                assert!(
                    store.write("metrics", vec![point(second as i64)]),
                    "case {case:?}: a write into the instance's own bucket must be taken"
                );
            }
            let held = store.points("metrics");
            let expected_len = written.min(MAX_POINTS_PER_BUCKET);
            assert_eq!(
                held.len(),
                expected_len,
                "case {case:?}: expected {expected_len} points, got {}",
                held.len()
            );
            let newest = held.last().expect("a non-empty bucket");
            assert_eq!(
                newest,
                &point(written as i64 - 1),
                "case {case:?}: the newest point must be the last written, got {newest:?}"
            );
        }
    }

    /// RULE: a write names a bucket the instance HAS; a name it does not have stores
    /// nothing and mints no bucket, so the store's size cannot be grown by inventing names.
    #[test]
    fn a_write_mints_no_bucket() {
        let mut store = store();
        for case in ["other", "", "metrics2", "METRICS"] {
            assert!(
                !store.write(
                    case,
                    vec![Point {
                        measurement: "cpu".to_string(),
                        tags: BTreeMap::new(),
                        fields: vec![("value".to_string(), FieldValue::Integer(1))],
                        timestamp_ns: 1,
                    }]
                ),
                "case {case:?}: a bucket this instance does not have must take no write"
            );
            assert!(
                !store.has_bucket(case),
                "case {case:?}: the refused write must not have minted the bucket"
            );
            assert!(
                store.points(case).is_empty(),
                "case {case:?}: nothing may be stored under it"
            );
        }
    }

    /// RULE: a session is recognised once minted, two mints never collide, and the store
    /// holds no more than its bound.
    #[test]
    fn a_minted_session_is_recognised_and_the_set_is_bounded() {
        let mut store = store();
        let mut minted: Vec<String> = Vec::new();
        for index in 0..MAX_SESSIONS + 3 {
            let id = store.open_session();
            assert!(
                store.has_session(&id),
                "case \"session {index}\": a freshly minted session must be recognised"
            );
            assert!(
                !minted.contains(&id),
                "case \"session {index}\": session id {id:?} was minted twice"
            );
            minted.push(id);
        }
        let live = minted
            .iter()
            .filter(|id| store.has_session(id))
            .count();
        assert_eq!(
            live, MAX_SESSIONS,
            "case \"past the bound\": expected {MAX_SESSIONS} live sessions, got {live}"
        );
        assert!(
            !store.has_session("not-a-session"),
            "case \"a session nobody minted\": must not be recognised"
        );
    }
}
