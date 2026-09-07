//! Conversions between the API's canonical asset strings, Horizon's asset
//! representation and XDR.

use stellar_xdr as xdr;

use crate::{Error, Result};

pub use reserve_horizon::Asset;

pub fn parse_asset(s: &str) -> Result<Asset> {
    Asset::parse(s).ok_or_else(|| Error::Asset(s.to_string()))
}

pub fn account_id(g: &str) -> Result<xdr::AccountId> {
    let pk = stellar_strkey::ed25519::PublicKey::from_string(g)
        .map_err(|_| Error::Account(g.to_string()))?;
    Ok(xdr::AccountId(xdr::PublicKey::PublicKeyTypeEd25519(
        xdr::Uint256(pk.0),
    )))
}

pub fn muxed_account(g: &str) -> Result<xdr::MuxedAccount> {
    let pk = stellar_strkey::ed25519::PublicKey::from_string(g)
        .map_err(|_| Error::Account(g.to_string()))?;
    Ok(xdr::MuxedAccount::Ed25519(xdr::Uint256(pk.0)))
}

pub fn to_xdr_asset(asset: &Asset) -> Result<xdr::Asset> {
    Ok(match asset {
        Asset::Native => xdr::Asset::Native,
        Asset::Credit { code, issuer } => {
            let issuer = account_id(issuer)?;
            if code.len() <= 4 {
                let mut buf = [0u8; 4];
                buf[..code.len()].copy_from_slice(code.as_bytes());
                xdr::Asset::CreditAlphanum4(xdr::AlphaNum4 {
                    asset_code: xdr::AssetCode4(buf),
                    issuer,
                })
            } else if code.len() <= 12 {
                let mut buf = [0u8; 12];
                buf[..code.len()].copy_from_slice(code.as_bytes());
                xdr::Asset::CreditAlphanum12(xdr::AlphaNum12 {
                    asset_code: xdr::AssetCode12(buf),
                    issuer,
                })
            } else {
                return Err(Error::Asset(code.clone()));
            }
        }
    })
}

pub fn to_change_trust_asset(asset: &Asset) -> Result<xdr::ChangeTrustAsset> {
    Ok(match to_xdr_asset(asset)? {
        xdr::Asset::Native => xdr::ChangeTrustAsset::Native,
        xdr::Asset::CreditAlphanum4(a) => xdr::ChangeTrustAsset::CreditAlphanum4(a),
        xdr::Asset::CreditAlphanum12(a) => xdr::ChangeTrustAsset::CreditAlphanum12(a),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";

    #[test]
    fn credit_assets_pad_their_code() {
        let a = to_xdr_asset(&parse_asset(&format!("USDC:{G}")).unwrap()).unwrap();
        match a {
            xdr::Asset::CreditAlphanum4(a) => assert_eq!(&a.asset_code.0, b"USDC"),
            _ => panic!("expected alphanum4"),
        }
        let long = to_xdr_asset(&parse_asset(&format!("LONGASSET1:{G}")).unwrap()).unwrap();
        assert!(matches!(long, xdr::Asset::CreditAlphanum12(_)));
    }

    #[test]
    fn rejects_bad_accounts() {
        assert!(account_id("not-an-account").is_err());
    }
}
