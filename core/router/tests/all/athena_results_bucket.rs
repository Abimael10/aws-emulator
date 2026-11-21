// SPDX-License-Identifier: CC0-1.0

//! The results bucket a cross-service Athena suite's queries write into, held in the object
//! store its engine carries.
//!
//! Athena verifies the resolved output location before it accepts a submission — a location
//! naming a bucket the account does not hold is one it can neither verify nor create — so a
//! suite that configures a results location has to have the bucket that location names,
//! exactly as a client does before it runs its first query. A suite that mounts Amazon S3
//! itself creates that bucket over S3's own wire; the ones here do not mount it, so they hold
//! it in the store directly.
//!
//! It is SETUP, not a suite: there is no test here, and nothing in it asserts anything about
//! any service.

use std::sync::Arc;

use service_com_amazonaws_s3_amazons3::state as s3_state;

/// The Region a results bucket is created in.
const REGION: &str = "us-east-1";

/// Hold the bucket the output location `location` names in the object store `engine` carries.
///
/// `location` is the `s3://<bucket>/<prefix>/` a suite configures; a location naming no
/// bucket is a mistake in the suite and panics. Idempotent: a bucket already there is left
/// as it is.
pub(crate) fn hold_results_bucket(engine: &Arc<engine::InMemoryEngine>, location: &str) {
    let bucket = location
        .strip_prefix("s3://")
        .map(|rest| rest.split('/').next().unwrap_or_default())
        .filter(|bucket| !bucket.is_empty())
        .unwrap_or_else(|| {
            panic!("setup: expected an s3:// results location, got {location:?}")
        });
    let state = s3_state::service_state(&**engine)
        .expect("setup: the object store is reachable on this engine");
    let mut store = state.lock();
    if store.bucket(bucket).is_none() {
        store.create_bucket(bucket, REGION);
    }
}
