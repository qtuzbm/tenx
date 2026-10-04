use anchor_lang::prelude::*;
use crate::{constants::*, error::VaultError};

#[zero_copy]
#[derive(Default)]
pub struct HeapEntry {
    pub tokens: u64,
    pub idx: u32,
    pub _pad: u32,
}
impl HeapEntry {
    /// Max-heap priority: more shares first; equal shares, earlier buyer first (same as the EVM key).
    #[inline]
    pub fn above(&self, o: &HeapEntry) -> bool { self.tokens > o.tokens || (self.tokens == o.tokens && self.idx < o.idx) }
}

/// One vault per ticket price. Created by the client with the full size, then `initialize`.
#[account(zero_copy)]
pub struct Vault {
    pub mint: Pubkey,
    pub bonding_curve: Pubkey,
    pub creator: Pubkey,
    pub ticket: u64,
    pub target_mult: u64,
    pub lock_period: i64,
    pub virtual_native: u64,
    pub virtual_token: u64,
    pub reserve_native: u64,
    pub reserve_token: u64,
    pub real_native: u64,
    pub burn_reserve: u64,
    pub total_burned_native: u64,
    pub total_burned_tokens: u64,
    pub burn_count: u64,
    pub burn_released_native: u64,
    pub total_in: u64,
    pub total_out: u64,
    pub auto_sold_count: u64,
    pub exited_count: u64,
    pub live_count: u64,
    pub positions_len: u64,
    pub created_at: i64,
    /// last successful burn_buy (starts at created_at); see release_burn_reserve
    pub last_burn_at: i64,
    pub max_settle_per_buy: u32,
    pub heap_len: u32,
    pub burn_bps: u16,
    pub buyer_bump: u8,
    pub _pad: [u8; 5],
    pub heap: [HeapEntry; HEAP_CAP],
}

#[account]
#[derive(InitSpace)]
pub struct Position {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub idx: u64,
    pub tokens: u64,
    pub paid: u64,
    pub received: u64,
    pub bought_at: i64,
    pub closed_at: i64,
    pub status: u8,
    pub bump: u8,
}

pub const STATUS_LIVE: u8 = 1;
pub const STATUS_AUTO_SOLD: u8 = 2;
pub const STATUS_EXITED: u8 = 3;

fn mul_div_ceil(a: u64, b: u64, d: u64) -> Result<u64> {
    require!(d > 0, VaultError::Overflow);
    let p = (a as u128).checked_mul(b as u128).ok_or(VaultError::Overflow)?;
    let q = p.checked_add(d as u128 - 1).ok_or(VaultError::Overflow)? / d as u128;
    u64::try_from(q).map_err(|_| VaultError::Overflow.into())
}

impl Vault {
    /// Shares for `native_in` (rounded down; the pool keeps the rounding, as in the EVM vault).
    pub fn quote_buy(&self, native_in: u64) -> Result<u64> {
        let rn = self.reserve_native;
        let after = rn.checked_add(native_in).ok_or(VaultError::Overflow)?;
        let k_div = mul_div_ceil(rn, self.reserve_token, after)?;
        self.reserve_token.checked_sub(k_div).ok_or(VaultError::Overflow.into())
    }
    /// Native out for selling `tokens` back (rounded down, capped at the real pool).
    pub fn quote_sell(&self, tokens: u64) -> Result<u64> {
        let after = self.reserve_token.checked_add(tokens).ok_or(VaultError::Overflow)?;
        let k_div = mul_div_ceil(self.reserve_native, self.reserve_token, after)?;
        let out = self.reserve_native.checked_sub(k_div).ok_or(VaultError::Overflow)?;
        Ok(out.min(self.real_native))
    }
    pub fn target(&self) -> Result<u64> { self.ticket.checked_mul(self.target_mult).ok_or(VaultError::Overflow.into()) }

    pub fn heap_push(&mut self, e: HeapEntry) -> Result<()> {
        let n = self.heap_len as usize;
        require!(n < HEAP_CAP, VaultError::VaultFull);
        self.heap_len += 1;
        self.sift_up(n, e);
        Ok(())
    }
    pub fn heap_top(&self) -> Option<HeapEntry> { if self.heap_len == 0 { None } else { Some(self.heap[0]) } }
    /// Remove the entry for position `idx` (linear scan; at most HEAP_CAP compares).
    pub fn heap_remove(&mut self, idx: u32) -> Result<()> {
        let n = self.heap_len as usize;
        let slot = (0..n).find(|&s| self.heap[s].idx == idx).ok_or(VaultError::AccountMismatch)?;
        let last = self.heap[n - 1];
        self.heap[n - 1] = HeapEntry::default();
        self.heap_len -= 1;
        if slot == n - 1 { return Ok(()); }
        let landed = self.sift_down(slot, last);
        if landed == slot { self.sift_up(slot, last); }
        Ok(())
    }
    fn sift_up(&mut self, mut slot: usize, e: HeapEntry) {
        while slot != 0 {
            let parent = (slot - 1) >> 1;
            let p = self.heap[parent];
            if !e.above(&p) { break; }
            self.heap[slot] = p;
            slot = parent;
        }
        self.heap[slot] = e;
    }
    fn sift_down(&mut self, mut slot: usize, e: HeapEntry) -> usize {
        let n = self.heap_len as usize;
        loop {
            let l = (slot << 1) + 1;
            if l >= n { break; }
            let mut best = l;
            let r = l + 1;
            if r < n && self.heap[r].above(&self.heap[l]) { best = r; }
            if !self.heap[best].above(&e) { break; }
            self.heap[slot] = self.heap[best];
            slot = best;
        }
        self.heap[slot] = e;
        slot
    }
}
