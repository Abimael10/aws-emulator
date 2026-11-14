// SPDX-License-Identifier: CC0-1.0

//! Which KMS key a backup vault created with no `EncryptionKeyArn` is encrypted
//! under.
//!
//! # The rule this module implements
//!
//! "If you do not specify a key, Backup creates a KMS key for you by default"
//! (model doc for `CreateBackupVaultInput$EncryptionKeyArn`), and the key it
//! creates is the Amazon Web Services managed key of this service for the
//! account and Region — the key behind the predefined [`AWS_MANAGED_KEY_ALIAS`].
//! `DescribeBackupVault` and `ListBackupVaults` then report THAT key's ARN, so
//! the identifier a vault publishes has to be one KMS itself resolves: a reader
//! that takes `EncryptionKeyArn` to `kms:DescribeKey` gets the key's metadata
//! rather than a key id that names nothing.
//!
//! So the key is resolved against the KMS store on the same shared engine
//! ([`default_encryption_key_arn`]) rather than minted inside this service's own
//! state. The alias is a PREDEFINED Amazon Web Services one: it "exists in every
//! account and Region" carrying no target key until something asks for it, when
//! "KMS associates the alias with an Amazon Web Services managed key" — so
//! asking for it is what gives it a key, and every vault created without one in
//! that account and Region resolves the alias to the SAME key.
//!
//! Only a vault CREATE resolves, so no read materialises a key or changes what a
//! later caller sees.

use std::time::SystemTime;

use super::ComAmazonawsBackupCryocontrollerusermanagerState;

/// The alias KMS holds the Amazon Web Services managed key of this service
/// under, in the `alias/aws/<service-name>` form the KMS model documents for
/// such a key; the service name is this service's own endpoint prefix
/// (`smithy_query(query="service_info")`: `backup`).
pub const AWS_MANAGED_KEY_ALIAS: &str = "alias/aws/backup";

/// The ARN of the key a vault created in `region` with no `EncryptionKeyArn` is
/// encrypted under: the Amazon Web Services managed key of this service, as KMS
/// resolves [`AWS_MANAGED_KEY_ALIAS`] — and that resolution is what creates the
/// key the first time a vault of that Region needs it.
///
/// The ONE place a vault's default key is decided, shared by the vault creates,
/// so two vaults of one Region cannot report two different default keys and the
/// key every one of them names is a key KMS holds.
///
/// `store` is this service's own state, used only when the KMS store cannot be
/// reached — an engine that is not the shared in-memory one, or a KMS already at
/// its own key ceiling: nothing can then resolve the alias, so the vault records
/// the key this service keeps for the Region
/// ([`ComAmazonawsBackupCryocontrollerusermanagerState::default_encryption_key_arn`])
/// rather than being refused a create AWS serves.
///
/// LOCKING: this takes the KMS store's lock and no other, and no KMS operation
/// reaches into this service's store, so the two can never be taken in opposite
/// orders.
pub fn default_encryption_key_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &mut ComAmazonawsBackupCryocontrollerusermanagerState,
    region: &str,
    now: SystemTime,
) -> String {
    match aws_managed_key_arn(engine, region) {
        Some(arn) => arn,
        None => store.default_encryption_key_arn(region, now),
    }
}

/// The ARN KMS reports for the Amazon Web Services managed key of this service
/// at `region`'s endpoint, or `None` when no KMS store on this engine can answer.
fn aws_managed_key_arn<Eng: Send + Sync + 'static>(engine: &Eng, region: &str) -> Option<String> {
    use ::service_com_amazonaws_kms_trentservice::state as kms_store;

    let keys = kms_store::service_state(engine)?;
    let alias = kms_store::alias_named_by(region, AWS_MANAGED_KEY_ALIAS)?;
    let mut keys = keys.lock();
    keys.associate_predefined_aws_alias(region, &alias)
        .map(|key| key.arn.clone())
}
