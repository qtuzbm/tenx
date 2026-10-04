use anchor_lang::prelude::*;

#[error_code]
pub enum VaultError {
    #[msg("Bad vault parameters")]
    BadParams,
    #[msg("Shares out below the buyer's minimum")]
    Slippage,
    #[msg("Exit payout below the owner's minimum")]
    ExitSlippage,
    #[msg("Position is not live")]
    NotLive,
    #[msg("Position is still locked")]
    StillLocked,
    #[msg("Vault has the maximum number of live positions")]
    VaultFull,
    #[msg("Arithmetic overflow")]
    Overflow,
    #[msg("Account is not the pump.fun bonding curve of this vault's mint")]
    BadBondingCurve,
    #[msg("Coin is not paired with SOL")]
    NotSolPaired,
    #[msg("Mint is not an SPL Token or Token-2022 mint")]
    BadMint,
    #[msg("Coin has left its bonding curve")]
    Graduated,
    #[msg("Coin is still on its bonding curve and burns are not stale")]
    NotGraduated,
    #[msg("Nothing to burn")]
    NothingToBurn,
    #[msg("Burn minimum is below the allowed floor")]
    BurnMinTooLow,
    #[msg("Account does not match the vault")]
    AccountMismatch,
    #[msg("Vault would hold less than its liabilities")]
    Insolvent,
    #[msg("Vault has sold tickets or holds funds")]
    NotEmpty,
}
