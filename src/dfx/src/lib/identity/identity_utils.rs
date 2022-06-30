use crate::lib::environment::Environment;
use crate::lib::error::DfxResult;

use anyhow::{bail, Context};
use fn_error_context::context;
use ic_agent::identity::BasicIdentity;
use ic_types::principal::Principal;
use openssl::ec::EcKey;
use openssl::nid::Nid;

#[derive(Debug, PartialEq)]
pub enum CallSender {
    SelectedId,
    Wallet(Principal),
}

// Determine whether the selected Identity
// or the provided wallet canister ID should be the Sender of the call.
#[context("Failed to determine call sender.")]
pub async fn call_sender(_env: &dyn Environment, wallet: &Option<String>) -> DfxResult<CallSender> {
    let sender = if let Some(id) = wallet {
        CallSender::Wallet(
            Principal::from_text(&id)
                .with_context(|| format!("Failed to read principal from {:?}.", id))?,
        )
    } else {
        CallSender::SelectedId
    };
    Ok(sender)
}

#[context("Failed to validate pem file.")]
pub fn validate_pem_file(pem_content: &[u8]) -> DfxResult {
    if pem_content.starts_with(b"-----BEGIN EC PARAMETERS-----")
        || pem_content.starts_with(b"-----BEGIN EC PRIVATE KEY-----")
    {
        let private_key =
            EcKey::private_key_from_pem(pem_content).context("Cannot decode PEM file content.")?;
        let named_curve = private_key.group().curve_name();
        let is_secp256k1 = named_curve == Some(Nid::SECP256K1);
        if !is_secp256k1 {
            bail!("This functionality is currently restricted to secp256k1 private keys.");
        }
    } else {
        // The PEM file generated by `dfx new` don't have EC PARAMETERS header and the curve is Ed25519
        let _basic_identity = BasicIdentity::from_pem(pem_content)
            .context("Invalid Ed25519 private key in PEM file")?;
    }
    Ok(())
}
