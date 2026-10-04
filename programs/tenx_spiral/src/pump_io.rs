use anchor_lang::prelude::*;
use crate::{constants::pump, error::VaultError};

pub struct CurveView { pub vtoken: u64, pub vquote: u64, pub complete: bool }

fn rd_u64(d: &[u8], o: usize) -> Result<u64> {
    let b: [u8; 8] = d.get(o..o + 8).ok_or(VaultError::BadBondingCurve)?.try_into().map_err(|_| VaultError::BadBondingCurve)?;
    Ok(u64::from_le_bytes(b))
}

/// The pump.fun bonding curve of `mint`: right address, owned by pump.fun, right discriminator, SOL-paired.
pub fn read_curve(curve: &AccountInfo, mint: &Pubkey) -> Result<CurveView> {
    let (want, _) = Pubkey::find_program_address(&[pump::BONDING_CURVE_SEED, mint.as_ref()], &pump::PROGRAM_ID);
    require_keys_eq!(curve.key(), want, VaultError::BadBondingCurve);
    require_keys_eq!(*curve.owner, pump::PROGRAM_ID, VaultError::BadBondingCurve);
    let d = curve.try_borrow_data()?;
    require!(d.len() > pump::OFF_COMPLETE && d[..8] == pump::BONDING_CURVE_DISC, VaultError::BadBondingCurve);
    if d.len() >= pump::OFF_QUOTE_MINT + 32 {
        let q = Pubkey::try_from(&d[pump::OFF_QUOTE_MINT..pump::OFF_QUOTE_MINT + 32]).map_err(|_| VaultError::BadBondingCurve)?;
        require!(q == Pubkey::default() || q == pump::NATIVE_MINT, VaultError::NotSolPaired);
    }
    Ok(CurveView { vtoken: rd_u64(&d, pump::OFF_VTOKEN)?, vquote: rd_u64(&d, pump::OFF_VQUOTE)?, complete: d[pump::OFF_COMPLETE] != 0 })
}

/// Raw SPL / Token-2022 base-layout reads (identical offsets in both programs).
pub fn token_amount(acc: &AccountInfo) -> Result<u64> { rd_u64(&acc.try_borrow_data()?, 64) }
pub fn mint_decimals(mint: &AccountInfo) -> Result<u8> {
    let d = mint.try_borrow_data()?;
    d.get(44).copied().ok_or(VaultError::BadMint.into())
}
