//! TenX 10x vault on Solana — a port of SpiralVault 3.2.0 (EVM) for a coin launched on pump.fun.
//!
//! Same rules as the EVM vault: one ticket per wallet at a fixed price; the ticket minus the burn share
//! buys vault shares on the vault's own constant-product curve; a position is paid out when selling its
//! shares on that curve returns `ticket * target_mult`; the position with the most shares goes first
//! (ties: earlier buyer); anyone can exit at the curve price after the lock period (0 = any time).
//! No owner, no admin, no pause, no withdraw path. Native SOL leaves the vault only as payouts, exits
//! and the burn buy.
//!
//! Solana difference: a failed cross-program call aborts the whole transaction, so the pump.fun buy is
//! NOT inside `buy`. `buy` sends the burn share to the vault's buyer PDA (a system account only this
//! program can sign for) and records it as `burn_reserve`; `burn_buy` (anyone) spends exactly that on the
//! coin's bonding curve and burns everything bought. Ticket sales never depend on pump.fun.
//! Once the coin leaves its curve the burn leg stops (as in the EVM vault): new tickets go fully into the
//! pool and `release_burn_reserve` moves the reserve from the buyer PDA into the pool.
pub mod constants;
pub mod error;
pub mod pump_io;
pub mod state;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{instruction::{AccountMeta, Instruction}, program::invoke_signed};
use anchor_spl::associated_token::{self, AssociatedToken};
use anchor_spl::token_interface::{self, BurnChecked, TokenInterface};

pub use constants::*;
pub use error::VaultError;
pub use state::*;

declare_id!("TenXUym3KteNymLHEQwffEQdRVS3huUFkbkrggaXwLK");

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct InitArgs {
    pub ticket: u64,
    pub target_mult: u64,
    pub lock_period: i64,
    pub virtual_native: u64,
    pub virtual_token: u64,
    pub max_settle_per_buy: u32,
    pub burn_bps: u16,
}

#[event] pub struct VaultCreated { pub vault: Pubkey, pub mint: Pubkey, pub ticket: u64, pub target_mult: u64, pub burn_bps: u16 }
#[event] pub struct Bought { pub vault: Pubkey, pub idx: u64, pub buyer: Pubkey, pub paid: u64, pub tokens: u64, pub to_burn: u64 }
#[event] pub struct AutoSold { pub vault: Pubkey, pub idx: u64, pub owner: Pubkey, pub tokens: u64, pub received: u64 }
#[event] pub struct Exited { pub vault: Pubkey, pub idx: u64, pub owner: Pubkey, pub tokens: u64, pub received: u64 }
#[event] pub struct Burned { pub vault: Pubkey, pub native_spent: u64, pub tokens_burned: u64 }
#[event] pub struct BurnReleased { pub vault: Pubkey, pub native: u64 }

#[program]
pub mod tenx_spiral {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, a: InitArgs) -> Result<()> {
        let x = &ctx.accounts;
        setup_vault(&x.vault, &x.creator, &x.burn_buyer, ctx.bumps.burn_buyer, &x.mint, &x.bonding_curve, a)
    }

    /// Open the standard vault of `tier` for a pump.fun coin. Anyone can call it (usually the first ticket buyer,
    /// in the same transaction as their buy); the caller pays the vault's rent and gets it back if the vault is
    /// closed unused. The address is [VAULT_SEED, mint, tier] and every parameter is fixed by the tier.
    pub fn open_vault(ctx: Context<OpenVault>, tier: u8) -> Result<()> {
        require!((tier as usize) < TIER_TICKETS.len(), VaultError::BadParams);
        let ticket = TIER_TICKETS[tier as usize];
        let a = InitArgs { ticket, target_mult: STD_TARGET_MULT, lock_period: 0, virtual_native: 2 * ticket, virtual_token: STD_VIRTUAL_TOKEN,
            max_settle_per_buy: STD_MAX_SETTLE, burn_bps: STD_BURN_BPS };
        let x = &ctx.accounts;
        setup_vault(&x.vault, &x.payer, &x.burn_buyer, ctx.bumps.burn_buyer, &x.mint, &x.bonding_curve, a)
    }

    /// Buy this wallet's one ticket. `remaining_accounts` = [position, owner] pairs for the positions at the
    /// top of the payout order (the site passes them); settlement stops at the first one not supplied.
    pub fn buy<'info>(ctx: Context<'info, Buy<'info>>, min_shares_out: u64) -> Result<()> {
        let vault_key = ctx.accounts.vault.key();
        let (ticket, burn_bps, max_settle, mint, curve_key) = {
            let v = ctx.accounts.vault.load()?;
            (v.ticket, v.burn_bps, v.max_settle_per_buy, v.mint, v.bonding_curve)
        };
        require_keys_eq!(ctx.accounts.bonding_curve.key(), curve_key, VaultError::AccountMismatch);
        let curve = pump_io::read_curve(&ctx.accounts.bonding_curve, &mint)?;
        let to_burn = if burn_bps > 0 && !curve.complete { u64::try_from(ticket as u128 * burn_bps as u128 / BPS as u128).map_err(|_| VaultError::Overflow)? } else { 0 };
        let to_pool = ticket - to_burn;
        anchor_lang::system_program::transfer(
            CpiContext::new(anchor_lang::system_program::ID, anchor_lang::system_program::Transfer {
                from: ctx.accounts.buyer.to_account_info(), to: ctx.accounts.vault.to_account_info() }),
            to_pool)?;
        if to_burn > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new(anchor_lang::system_program::ID, anchor_lang::system_program::Transfer {
                    from: ctx.accounts.buyer.to_account_info(), to: ctx.accounts.burn_buyer.to_account_info() }),
                to_burn)?;
        }
        let now = Clock::get()?.unix_timestamp;
        let (idx, out) = {
            let mut v = ctx.accounts.vault.load_mut()?;
            let out = v.quote_buy(to_pool)?;
            require!(out > 0, VaultError::BadParams);
            require!(out >= min_shares_out, VaultError::Slippage);
            let idx = v.positions_len;
            require!(idx < u32::MAX as u64, VaultError::VaultFull);
            v.heap_push(HeapEntry { tokens: out, idx: idx as u32, _pad: 0 })?;
            v.reserve_native = v.reserve_native.checked_add(to_pool).ok_or(VaultError::Overflow)?;
            v.reserve_token = v.reserve_token.checked_sub(out).ok_or(VaultError::Overflow)?;
            v.real_native = v.real_native.checked_add(to_pool).ok_or(VaultError::Overflow)?;
            v.burn_reserve = v.burn_reserve.checked_add(to_burn).ok_or(VaultError::Overflow)?;
            v.total_in = v.total_in.checked_add(ticket).ok_or(VaultError::Overflow)?;
            v.positions_len += 1;
            v.live_count += 1;
            (idx, out)
        };
        let p = &mut ctx.accounts.position;
        p.vault = vault_key;
        p.owner = ctx.accounts.buyer.key();
        p.idx = idx;
        p.tokens = out;
        p.paid = ticket;
        p.received = 0;
        p.bought_at = now;
        p.closed_at = 0;
        p.status = STATUS_LIVE;
        p.bump = ctx.bumps.position;
        emit!(Bought { vault: vault_key, idx, buyer: p.owner, paid: ticket, tokens: out, to_burn });
        settle_inner(&ctx.accounts.vault, ctx.remaining_accounts, max_settle, now)?;
        assert_solvent(&ctx.accounts.vault)?;
        assert_burn_backed(&ctx.accounts.vault, &ctx.accounts.burn_buyer.to_account_info())
    }

    /// Anyone: pay out positions that have reached the target, most shares first, up to `max`.
    pub fn settle<'info>(ctx: Context<'info, Settle<'info>>, max: u32) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        settle_inner(&ctx.accounts.vault, ctx.remaining_accounts, max.min(MAX_SETTLE_CAP), now)?;
        assert_solvent(&ctx.accounts.vault)
    }

    /// Owner: sell the position back to the vault curve (after the lock period).
    pub fn exit(ctx: Context<Exit>, min_native_out: u64) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let p = &mut ctx.accounts.position;
        require!(p.status == STATUS_LIVE, VaultError::NotLive);
        let out = {
            let mut v = ctx.accounts.vault.load_mut()?;
            require!(now >= p.bought_at.saturating_add(v.lock_period), VaultError::StillLocked);
            let out = v.quote_sell(p.tokens)?;
            require!(out >= min_native_out, VaultError::ExitSlippage);
            close_position(&mut v, p, out, STATUS_EXITED, now)?;
            out
        };
        pay(&ctx.accounts.vault.to_account_info(), &ctx.accounts.owner.to_account_info(), out)?;
        emit!(Exited { vault: ctx.accounts.vault.key(), idx: p.idx, owner: p.owner, tokens: p.tokens, received: out });
        assert_solvent(&ctx.accounts.vault)
    }

    /// Anyone: spend the burn reserve on the coin's pump.fun bonding curve and burn every token bought.
    /// `remaining_accounts` are appended to the pump.fun instruction as given (pump.fun adds trailing
    /// accounts over time — e.g. bonding-curve-v2 and a buyback fee recipient since 2026-04-28 — so the
    /// client supplies whatever the current pump.fun interface requires). They never get signer rights.
    pub fn burn_buy<'info>(ctx: Context<'info, BurnBuy<'info>>, min_tokens_out: u64) -> Result<()> {
        let vault_key = ctx.accounts.vault.key();
        let (amt, mint, curve_key, buyer_bump) = {
            let v = ctx.accounts.vault.load()?;
            (v.burn_reserve, v.mint, v.bonding_curve, v.buyer_bump)
        };
        require!(amt > 0, VaultError::NothingToBurn);
        require_keys_eq!(ctx.accounts.mint.key(), mint, VaultError::AccountMismatch);
        require_keys_eq!(ctx.accounts.bonding_curve.key(), curve_key, VaultError::AccountMismatch);
        require_keys_eq!(*ctx.accounts.mint.to_account_info().owner, ctx.accounts.token_program.key(), VaultError::BadMint);
        let curve = pump_io::read_curve(&ctx.accounts.bonding_curve, &mint)?;
        require!(!curve.complete, VaultError::Graduated);
        // Fee-free curve quote; the caller's minimum must be at least BURN_MIN_OUT_BPS of it.
        let fee_free = (curve.vtoken as u128) - (curve.vtoken as u128 * curve.vquote as u128).div_ceil(curve.vquote as u128 + amt as u128);
        let floor = u64::try_from(fee_free * BURN_MIN_OUT_BPS as u128 / BPS as u128).map_err(|_| VaultError::Overflow)?;
        require!(min_tokens_out >= floor && min_tokens_out > 0, VaultError::BurnMinTooLow);

        // buyer = system-owned PDA that signs the pump.fun buy; its token account is its ATA for the mint.
        associated_token::create_idempotent(CpiContext::new(anchor_spl::associated_token::ID, associated_token::Create {
            payer: ctx.accounts.caller.to_account_info(),
            associated_token: ctx.accounts.buyer_token.to_account_info(),
            authority: ctx.accounts.buyer.to_account_info(),
            mint: ctx.accounts.mint.to_account_info(),
            system_program: ctx.accounts.system_program.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
        }))?;
        // The buyer PDA holds the reserve plus a rent-exempt float, and pays pump.fun's per-user volume account
        // once; the caller tops up the float if needed so the whole reserve goes into the buy.
        let rent = Rent::get()?;
        let mut need = amt.checked_add(rent.minimum_balance(0)).ok_or(VaultError::Overflow)?;
        if ctx.accounts.user_volume_accumulator.data_is_empty() { need += rent.minimum_balance(256); }
        let have = ctx.accounts.buyer.lamports();
        if have < need {
            anchor_lang::system_program::transfer(
                CpiContext::new(anchor_lang::system_program::ID, anchor_lang::system_program::Transfer {
                    from: ctx.accounts.caller.to_account_info(), to: ctx.accounts.buyer.to_account_info() }),
                need - have)?;
        }
        {
            let mut v = ctx.accounts.vault.load_mut()?;
            v.burn_reserve = 0;
        }

        let before = pump_io::token_amount(&ctx.accounts.buyer_token)?;
        let a = &ctx.accounts;
        let metas = vec![
            AccountMeta::new_readonly(a.pump_global.key(), false),
            AccountMeta::new(a.fee_recipient.key(), false),
            AccountMeta::new_readonly(a.mint.key(), false),
            AccountMeta::new(a.bonding_curve.key(), false),
            AccountMeta::new(a.associated_bonding_curve.key(), false),
            AccountMeta::new(a.buyer_token.key(), false),
            AccountMeta::new(a.buyer.key(), true),
            AccountMeta::new_readonly(a.system_program.key(), false),
            AccountMeta::new_readonly(a.token_program.key(), false),
            AccountMeta::new(a.creator_vault.key(), false),
            AccountMeta::new_readonly(a.event_authority.key(), false),
            AccountMeta::new_readonly(a.pump_program.key(), false),
            AccountMeta::new_readonly(a.global_volume_accumulator.key(), false),
            AccountMeta::new(a.user_volume_accumulator.key(), false),
            AccountMeta::new_readonly(a.fee_config.key(), false),
            AccountMeta::new_readonly(a.fee_program.key(), false),
        ];
        let mut metas = metas;
        for x in ctx.remaining_accounts.iter() {
            metas.push(if x.is_writable { AccountMeta::new(x.key(), false) } else { AccountMeta::new_readonly(x.key(), false) });
        }
        let mut data = Vec::with_capacity(25);
        data.extend_from_slice(&pump::BUY_EXACT_SOL_IN_DISC);
        data.extend_from_slice(&amt.to_le_bytes());
        data.extend_from_slice(&min_tokens_out.to_le_bytes());
        data.push(0); // track_volume: OptionBool(false)
        let ix = Instruction { program_id: pump::PROGRAM_ID, accounts: metas, data };
        let mut infos = vec![
            a.pump_global.to_account_info(), a.fee_recipient.to_account_info(), a.mint.to_account_info(),
            a.bonding_curve.to_account_info(), a.associated_bonding_curve.to_account_info(), a.buyer_token.to_account_info(),
            a.buyer.to_account_info(), a.system_program.to_account_info(), a.token_program.to_account_info(),
            a.creator_vault.to_account_info(), a.event_authority.to_account_info(), a.pump_program.to_account_info(),
            a.global_volume_accumulator.to_account_info(), a.user_volume_accumulator.to_account_info(),
            a.fee_config.to_account_info(), a.fee_program.to_account_info(),
        ];
        infos.extend(ctx.remaining_accounts.iter().cloned());
        let seeds: &[&[u8]] = &[BUYER_SEED, vault_key.as_ref(), &[buyer_bump]];
        invoke_signed(&ix, &infos, &[seeds])?;

        let after = pump_io::token_amount(&ctx.accounts.buyer_token)?;
        let got = after.checked_sub(before).ok_or(VaultError::Overflow)?;
        require!(got >= min_tokens_out, VaultError::Slippage);
        let decimals = pump_io::mint_decimals(&ctx.accounts.mint.to_account_info())?;
        token_interface::burn_checked(CpiContext::new_with_signer(ctx.accounts.token_program.key(), BurnChecked {
            mint: ctx.accounts.mint.to_account_info(),
            from: ctx.accounts.buyer_token.to_account_info(),
            authority: ctx.accounts.buyer.to_account_info(),
        }, &[seeds]), after, decimals)?;
        {
            let mut v = ctx.accounts.vault.load_mut()?;
            v.total_burned_native = v.total_burned_native.checked_add(amt).ok_or(VaultError::Overflow)?;
            v.total_burned_tokens = v.total_burned_tokens.checked_add(after).ok_or(VaultError::Overflow)?;
            v.burn_count += 1;
            v.last_burn_at = Clock::get()?.unix_timestamp;
        }
        emit!(Burned { vault: vault_key, native_spent: amt, tokens_burned: after });
        assert_solvent(&ctx.accounts.vault)?;
        assert_burn_backed(&ctx.accounts.vault, &ctx.accounts.buyer.to_account_info())
    }

    /// Anyone, after the coin has left its bonding curve — or when no burn has succeeded for BURN_STALE_SECS —
    /// the unburned reserve joins the vault pool.
    pub fn release_burn_reserve(ctx: Context<Release>) -> Result<()> {
        let (mint, curve_key) = { let v = ctx.accounts.vault.load()?; (v.mint, v.bonding_curve) };
        require_keys_eq!(ctx.accounts.bonding_curve.key(), curve_key, VaultError::AccountMismatch);
        let curve = pump_io::read_curve(&ctx.accounts.bonding_curve, &mint)?;
        let vault_key = ctx.accounts.vault.key();
        let (amt, bump, last) = { let v = ctx.accounts.vault.load()?; (v.burn_reserve, v.buyer_bump, v.last_burn_at) };
        require!(curve.complete || Clock::get()?.unix_timestamp >= last.saturating_add(BURN_STALE_SECS), VaultError::NotGraduated);
        require!(amt > 0, VaultError::NothingToBurn);
        let seeds: &[&[u8]] = &[BUYER_SEED, vault_key.as_ref(), &[bump]];
        anchor_lang::system_program::transfer(
            CpiContext::new_with_signer(anchor_lang::system_program::ID, anchor_lang::system_program::Transfer {
                from: ctx.accounts.burn_buyer.to_account_info(), to: ctx.accounts.vault.to_account_info() }, &[seeds]),
            amt)?;
        let mut v = ctx.accounts.vault.load_mut()?;
        v.burn_reserve = 0;
        v.real_native = v.real_native.checked_add(amt).ok_or(VaultError::Overflow)?;
        v.reserve_native = v.reserve_native.checked_add(amt).ok_or(VaultError::Overflow)?;
        v.burn_released_native = v.burn_released_native.checked_add(amt).ok_or(VaultError::Overflow)?;
        drop(v);
        emit!(BurnReleased { vault: ctx.accounts.vault.key(), native: amt });
        Ok(())
    }

    /// Anyone may close a vault that never sold a ticket and holds no pool or burn SOL. Its rent and the buyer PDA
    /// float go back to whoever opened it (`creator`), never anywhere else.
    pub fn close_empty_vault(ctx: Context<CloseEmpty>) -> Result<()> {
        let vault_key = ctx.accounts.vault.key();
        let bump = {
            let v = ctx.accounts.vault.load()?;
            require!(v.positions_len == 0 && v.live_count == 0 && v.total_in == 0 && v.real_native == 0 && v.burn_reserve == 0, VaultError::NotEmpty);
            require_keys_eq!(ctx.accounts.creator.key(), v.creator, VaultError::AccountMismatch);
            v.buyer_bump
        };
        let float = ctx.accounts.burn_buyer.lamports();
        if float > 0 {
            let seeds: &[&[u8]] = &[BUYER_SEED, vault_key.as_ref(), &[bump]];
            anchor_lang::system_program::transfer(
                CpiContext::new_with_signer(anchor_lang::system_program::ID, anchor_lang::system_program::Transfer {
                    from: ctx.accounts.burn_buyer.to_account_info(), to: ctx.accounts.creator.to_account_info() }, &[seeds]),
                float)?;
        }
        Ok(())
    }
}

fn setup_vault<'info>(vault: &AccountLoader<'info, Vault>, creator: &Signer<'info>, burn_buyer: &SystemAccount<'info>, buyer_bump: u8,
    mint: &UncheckedAccount<'info>, bonding_curve: &UncheckedAccount<'info>, a: InitArgs) -> Result<()> {
    require!(a.ticket > 0 && a.target_mult >= 2 && a.ticket.checked_mul(a.target_mult).is_some(), VaultError::BadParams);
    require!(a.virtual_native > 0 && a.virtual_native <= u64::MAX / 4 && a.virtual_token > 0 && a.virtual_token <= u64::MAX / 4, VaultError::BadParams);
    require!(a.max_settle_per_buy > 0 && a.max_settle_per_buy <= MAX_SETTLE_CAP && a.lock_period >= 0 && a.burn_bps <= MAX_BURN_BPS, VaultError::BadParams);
    require!(*mint.owner == anchor_spl::token::ID || *mint.owner == anchor_spl::token_2022::ID, VaultError::BadMint);
    require!(mint.data_len() >= 82, VaultError::BadMint);
    let curve = pump_io::read_curve(bonding_curve, &mint.key())?;
    require!(!curve.complete, VaultError::Graduated);
    let float = Rent::get()?.minimum_balance(0);
    let have = burn_buyer.lamports();
    if have < float {
        anchor_lang::system_program::transfer(
            CpiContext::new(anchor_lang::system_program::ID, anchor_lang::system_program::Transfer {
                from: creator.to_account_info(), to: burn_buyer.to_account_info() }),
            float - have)?;
    }
    let mut v = vault.load_init()?;
    v.mint = mint.key();
    v.bonding_curve = bonding_curve.key();
    v.creator = creator.key();
    v.ticket = a.ticket;
    v.target_mult = a.target_mult;
    v.lock_period = a.lock_period;
    v.virtual_native = a.virtual_native;
    v.virtual_token = a.virtual_token;
    v.reserve_native = a.virtual_native;
    v.reserve_token = a.virtual_token;
    v.max_settle_per_buy = a.max_settle_per_buy;
    v.burn_bps = a.burn_bps;
    v.buyer_bump = buyer_bump;
    v.created_at = Clock::get()?.unix_timestamp;
    v.last_burn_at = v.created_at;
    drop(v);
    emit!(VaultCreated { vault: vault.key(), mint: mint.key(), ticket: a.ticket, target_mult: a.target_mult, burn_bps: a.burn_bps });
    Ok(())
}

fn close_position(v: &mut Vault, p: &mut Position, out: u64, status: u8, now: i64) -> Result<()> {
    v.reserve_token = v.reserve_token.checked_add(p.tokens).ok_or(VaultError::Overflow)?;
    v.reserve_native = v.reserve_native.checked_sub(out).ok_or(VaultError::Overflow)?;
    v.real_native = v.real_native.checked_sub(out).ok_or(VaultError::Overflow)?;
    v.total_out = v.total_out.checked_add(out).ok_or(VaultError::Overflow)?;
    v.live_count -= 1;
    if status == STATUS_AUTO_SOLD { v.auto_sold_count += 1 } else { v.exited_count += 1 }
    v.heap_remove(p.idx as u32)?;
    p.status = status;
    p.received = out;
    p.closed_at = now;
    Ok(())
}

fn pay<'info>(from: &AccountInfo<'info>, to: &AccountInfo<'info>, amt: u64) -> Result<()> {
    if amt == 0 { return Ok(()); }
    let f = from.lamports().checked_sub(amt).ok_or(VaultError::Insolvent)?;
    let t = to.lamports().checked_add(amt).ok_or(VaultError::Overflow)?;
    **from.try_borrow_mut_lamports()? = f;
    **to.try_borrow_mut_lamports()? = t;
    Ok(())
}

/// Vault lamports must cover rent + pool after every instruction (the burn reserve sits on the buyer PDA).
fn assert_solvent(vault: &AccountLoader<Vault>) -> Result<()> {
    let ai = vault.to_account_info();
    let need = { let v = vault.load()?; Rent::get()?.minimum_balance(ai.data_len()).checked_add(v.real_native).ok_or(VaultError::Overflow)? };
    require!(ai.lamports() >= need, VaultError::Insolvent);
    Ok(())
}

/// The buyer PDA must hold at least the recorded burn reserve.
fn assert_burn_backed(vault: &AccountLoader<Vault>, buyer: &AccountInfo) -> Result<()> {
    let r = vault.load()?.burn_reserve;
    require!(buyer.lamports() >= r, VaultError::Insolvent);
    Ok(())
}

/// Pay out from the top of the order while the top position has reached the target.
/// `rest` holds [position, owner] pairs; the loop stops at the first top position not supplied.
fn settle_inner<'info>(vault: &AccountLoader<'info, Vault>, rest: &'info [AccountInfo<'info>], max: u32, now: i64) -> Result<u32> {
    let vault_key = vault.key();
    let vault_ai = vault.to_account_info();
    let mut n = 0u32;
    while n < max {
        let (top, out, target) = {
            let v = vault.load()?;
            let Some(top) = v.heap_top() else { break };
            (top, v.quote_sell(top.tokens)?, v.target()?)
        };
        if out < target { break; }
        let mut found = None;
        for pair in rest.chunks(2) {
            if pair.len() < 2 || *pair[0].owner != crate::ID || !pair[0].is_writable || !pair[1].is_writable { continue; }
            let Ok(pos) = Account::<Position>::try_from(&pair[0]) else { continue };
            if pos.vault == vault_key && pos.idx == top.idx as u64 && pos.status == STATUS_LIVE && pos.owner == pair[1].key() { found = Some((pos, &pair[1])); break; }
        }
        let Some((mut pos, owner_ai)) = found else { break };
        {
            let mut v = vault.load_mut()?;
            close_position(&mut v, &mut pos, out, STATUS_AUTO_SOLD, now)?;
        }
        pos.exit(&crate::ID)?;
        pay(&vault_ai, owner_ai, out)?;
        emit!(AutoSold { vault: vault_key, idx: pos.idx, owner: pos.owner, tokens: pos.tokens, received: out });
        n += 1;
    }
    Ok(n)
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    #[account(zero)]
    pub vault: AccountLoader<'info, Vault>,
    /// CHECK: system-owned PDA [BUYER_SEED, vault]; gets a rent-exempt float here so any burn share can land on it
    #[account(mut, seeds = [BUYER_SEED, vault.key().as_ref()], bump)]
    pub burn_buyer: SystemAccount<'info>,
    /// CHECK: owner and size checked in the handler (SPL Token or Token-2022 mint)
    pub mint: UncheckedAccount<'info>,
    /// CHECK: address, owner, discriminator and SOL pairing checked by pump_io::read_curve
    pub bonding_curve: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Buy<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,
    #[account(mut)]
    pub vault: AccountLoader<'info, Vault>,
    #[account(init, payer = buyer, space = 8 + Position::INIT_SPACE, seeds = [POS_SEED, vault.key().as_ref(), buyer.key().as_ref()], bump)]
    pub position: Account<'info, Position>,
    /// CHECK: system-owned PDA [BUYER_SEED, vault]; receives the burn share of the ticket
    #[account(mut, seeds = [BUYER_SEED, vault.key().as_ref()], bump)]
    pub burn_buyer: SystemAccount<'info>,
    /// CHECK: must equal vault.bonding_curve; read-only flag check in the handler
    pub bonding_curve: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(mut)]
    pub vault: AccountLoader<'info, Vault>,
}

#[derive(Accounts)]
pub struct Exit<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut)]
    pub vault: AccountLoader<'info, Vault>,
    #[account(mut, seeds = [POS_SEED, vault.key().as_ref(), owner.key().as_ref()], bump = position.bump, has_one = owner, has_one = vault)]
    pub position: Account<'info, Position>,
}

#[derive(Accounts)]
pub struct BurnBuy<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,
    #[account(mut)]
    pub vault: AccountLoader<'info, Vault>,
    /// CHECK: system-owned PDA [BUYER_SEED, vault]; signs the pump.fun buy and the burn
    #[account(mut, seeds = [BUYER_SEED, vault.key().as_ref()], bump)]
    pub buyer: SystemAccount<'info>,
    /// CHECK: must be the buyer's associated token account (created idempotently in the handler)
    #[account(mut, address = anchor_spl::associated_token::get_associated_token_address_with_program_id(&buyer.key(), &mint.key(), &token_program.key()))]
    pub buyer_token: UncheckedAccount<'info>,
    /// CHECK: must equal vault.mint
    #[account(mut)]
    pub mint: UncheckedAccount<'info>,
    /// CHECK: must equal vault.bonding_curve
    #[account(mut)]
    pub bonding_curve: UncheckedAccount<'info>,
    /// CHECK: validated by pump.fun
    #[account(mut)]
    pub associated_bonding_curve: UncheckedAccount<'info>,
    /// CHECK: validated by pump.fun
    pub pump_global: UncheckedAccount<'info>,
    /// CHECK: validated by pump.fun
    #[account(mut)]
    pub fee_recipient: UncheckedAccount<'info>,
    /// CHECK: validated by pump.fun
    #[account(mut)]
    pub creator_vault: UncheckedAccount<'info>,
    /// CHECK: validated by pump.fun
    pub event_authority: UncheckedAccount<'info>,
    /// CHECK: pump.fun program
    #[account(address = pump::PROGRAM_ID)]
    pub pump_program: UncheckedAccount<'info>,
    /// CHECK: validated by pump.fun
    pub global_volume_accumulator: UncheckedAccount<'info>,
    /// CHECK: validated by pump.fun (PDA of the buyer)
    #[account(mut)]
    pub user_volume_accumulator: UncheckedAccount<'info>,
    /// CHECK: validated by pump.fun
    pub fee_config: UncheckedAccount<'info>,
    /// CHECK: pump.fun fee program
    #[account(address = pump::FEE_PROGRAM_ID)]
    pub fee_program: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Release<'info> {
    #[account(mut)]
    pub vault: AccountLoader<'info, Vault>,
    /// CHECK: must equal vault.bonding_curve
    pub bonding_curve: UncheckedAccount<'info>,
    /// CHECK: system-owned PDA [BUYER_SEED, vault] holding the reserve
    #[account(mut, seeds = [BUYER_SEED, vault.key().as_ref()], bump)]
    pub burn_buyer: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(tier: u8)]
pub struct OpenVault<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(init, payer = payer, space = 8 + std::mem::size_of::<Vault>(), seeds = [VAULT_SEED, mint.key().as_ref(), &[tier]], bump)]
    pub vault: AccountLoader<'info, Vault>,
    /// CHECK: system-owned PDA [BUYER_SEED, vault]; gets a rent-exempt float here so any burn share can land on it
    #[account(mut, seeds = [BUYER_SEED, vault.key().as_ref()], bump)]
    pub burn_buyer: SystemAccount<'info>,
    /// CHECK: owner and size checked in setup_vault (SPL Token or Token-2022 mint)
    pub mint: UncheckedAccount<'info>,
    /// CHECK: address, owner, discriminator and SOL pairing checked by pump_io::read_curve
    pub bonding_curve: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseEmpty<'info> {
    #[account(mut, close = creator)]
    pub vault: AccountLoader<'info, Vault>,
    /// CHECK: system-owned PDA [BUYER_SEED, vault]; its rent float goes back to the creator
    #[account(mut, seeds = [BUYER_SEED, vault.key().as_ref()], bump)]
    pub burn_buyer: SystemAccount<'info>,
    /// CHECK: must equal vault.creator (checked in the handler); receives the rent
    #[account(mut)]
    pub creator: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}
