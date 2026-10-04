use anchor_lang::prelude::*;

/// Solana port of SpiralVault 3.2.0. Same ticket / share-curve / settlement-order rules;
/// the burn leg buys the pump.fun coin through a separate permissionless instruction (see burn_buy).
pub const VERSION: &str = "3.3.0-sol.1";

pub const POS_SEED: &[u8] = b"pos";
pub const BUYER_SEED: &[u8] = b"buyer";
/// Standard vaults live at [VAULT_SEED, mint, tier]: one per coin per tier, opened by whoever gets there first.
pub const VAULT_SEED: &[u8] = b"vault";

/// Max live (unsettled, unexited) positions per vault. Closed positions leave the heap. Live positions grow faster
/// than 10x payouts (standard tier: first payout at ticket 15, about 30 payouts by ticket 400), so the cap is how long
/// a vault keeps selling: 128 fills at ticket ~138, 512 lasts past 400. 512 keeps the account (8.5 KB) inside the
/// 10 KB an instruction can create, so the first buyer can open the vault and buy in one transaction.
pub const HEAP_CAP: usize = 512;
pub const BPS: u64 = 10_000;
/// Same hard cap as the EVM vault: above 30% the 10x payout would be drained too far.
pub const MAX_BURN_BPS: u16 = 3_000;
/// Settlement work per buy is bounded; anyone can call settle() for more.
pub const MAX_SETTLE_CAP: u32 = 16;
/// burn_buy refuses a caller minimum below this share of the fee-free curve quote (sandwich bound).
pub const BURN_MIN_OUT_BPS: u64 = 9_000;
/// If no burn has succeeded for this long (pump.fun interface gone, nobody calling), anyone may move the
/// reserve into the pool instead of leaving it stuck.
pub const BURN_STALE_SECS: i64 = 30 * 24 * 3600;
/// Standard tiers for open_vault: ticket price in lamports. Everything else is fixed below, so nobody picks parameters.
pub const TIER_TICKETS: [u64; 2] = [100_000_000, 10_000_000];
pub const STD_TARGET_MULT: u64 = 10;
pub const STD_BURN_BPS: u16 = 1500;
pub const STD_MAX_SETTLE: u32 = 10;
pub const STD_VIRTUAL_TOKEN: u64 = 1_000_000_000_000_000;

pub mod pump {
    use super::*;
    pub const PROGRAM_ID: Pubkey = pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
    pub const FEE_PROGRAM_ID: Pubkey = pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
    pub const BONDING_CURVE_SEED: &[u8] = b"bonding-curve";
    pub const BONDING_CURVE_DISC: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
    pub const BUY_EXACT_SOL_IN_DISC: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];
    pub const NATIVE_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
    // BondingCurve layout (pump-public-docs idl/pump.json)
    pub const OFF_VTOKEN: usize = 8;
    pub const OFF_VQUOTE: usize = 16;
    pub const OFF_COMPLETE: usize = 48;
    pub const OFF_QUOTE_MINT: usize = 83;
}
