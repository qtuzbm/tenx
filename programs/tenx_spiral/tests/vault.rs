//! Vault rules without pump.fun: a synthetic bonding-curve account (right PDA, owner, discriminator) stands in.
use {
    anchor_lang::{prelude::Pubkey, solana_program::instruction::{AccountMeta, Instruction}, AccountDeserialize, InstructionData, ToAccountMetas},
    litesvm::LiteSVM,
    solana_account::Account,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    tenx_spiral::{constants::{pump, BUYER_SEED, POS_SEED}, state::{Position, Vault, STATUS_AUTO_SOLD, STATUS_EXITED, STATUS_LIVE}, InitArgs},
    std::collections::HashMap,
};

const SOL: u64 = 1_000_000_000;
const TICKET: u64 = SOL / 10;

fn program_bytes() -> Vec<u8> { std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/tenx_spiral.so")).unwrap() }

struct H { svm: LiteSVM, payer: Keypair, vault: Pubkey, mint: Pubkey, curve: Pubkey, owners: HashMap<u64, Keypair> }

fn bb(vault: &Pubkey) -> Pubkey { Pubkey::find_program_address(&[BUYER_SEED, vault.as_ref()], &tenx_spiral::ID).0 }
fn send(svm: &mut LiteSVM, ixs: &[Instruction], payer: &Keypair, extra: &[&Keypair]) -> Result<(), String> {
    let bh = svm.latest_blockhash();
    let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &bh);
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra);
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers).map_err(|e| e.to_string())?;
    let r = svm.send_transaction(tx).map(|_| ()).map_err(|e| format!("{:?} | {}", e.err, e.meta.logs.join(" / ")));
    svm.expire_blockhash();
    r
}

fn curve_bytes(complete: bool, quote_mint: Pubkey) -> Vec<u8> {
    let mut d = vec![0u8; 151];
    d[..8].copy_from_slice(&pump::BONDING_CURVE_DISC);
    d[8..16].copy_from_slice(&1_073_000_000_000_000u64.to_le_bytes());
    d[16..24].copy_from_slice(&30_000_000_000u64.to_le_bytes());
    d[48] = complete as u8;
    d[83..115].copy_from_slice(quote_mint.as_ref());
    d
}
fn put(svm: &mut LiteSVM, key: Pubkey, owner: Pubkey, data: Vec<u8>) {
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(key, Account { lamports, data, owner, executable: false, rent_epoch: 0 }).unwrap();
}
fn mint_bytes() -> Vec<u8> { let mut d = vec![0u8; 82]; d[44] = 6; d[45] = 1; d }

fn vault_state(svm: &LiteSVM, vault: &Pubkey) -> Vault {
    let a = svm.get_account(vault).unwrap();
    *bytemuck::from_bytes::<Vault>(&a.data[8..8 + std::mem::size_of::<Vault>()])
}
fn position(svm: &LiteSVM, key: &Pubkey) -> Position { let a = svm.get_account(key).unwrap(); Position::try_deserialize(&mut a.data.as_slice()).unwrap() }
fn pos_key(vault: &Pubkey, owner: &Pubkey) -> Pubkey { Pubkey::find_program_address(&[POS_SEED, vault.as_ref(), owner.as_ref()], &tenx_spiral::ID).0 }

fn args() -> InitArgs { InitArgs { ticket: TICKET, target_mult: 10, lock_period: 0, virtual_native: 2 * TICKET, virtual_token: 1_000_000_000_000_000, max_settle_per_buy: 10, burn_bps: 1500 } }

fn setup_with(a: InitArgs, complete: bool) -> Result<H, String> {
    let mut svm = LiteSVM::new();
    svm.add_program(tenx_spiral::ID, &program_bytes()).unwrap();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000 * SOL).unwrap();
    let mint = Pubkey::new_unique();
    put(&mut svm, mint, anchor_spl::token::ID, mint_bytes());
    let curve = Pubkey::find_program_address(&[pump::BONDING_CURVE_SEED, mint.as_ref()], &pump::PROGRAM_ID).0;
    put(&mut svm, curve, pump::PROGRAM_ID, curve_bytes(complete, Pubkey::default()));
    let vk = Keypair::new();
    let space = 8 + std::mem::size_of::<Vault>();
    let rent = svm.minimum_balance_for_rent_exemption(space);
    let create = solana_system_interface::instruction::create_account(&payer.pubkey(), &vk.pubkey(), rent, space as u64, &tenx_spiral::ID);
    let init = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Initialize { a }.data(),
        tenx_spiral::accounts::Initialize { creator: payer.pubkey(), vault: vk.pubkey(), burn_buyer: bb(&vk.pubkey()), mint, bonding_curve: curve, system_program: anchor_lang::system_program::ID }.to_account_metas(None));
    send(&mut svm, &[create, init], &payer, &[&vk])?;
    Ok(H { svm, payer, vault: vk.pubkey(), mint, curve, owners: HashMap::new() })
}
fn setup() -> H { setup_with(args(), false).unwrap() }

impl H {
    fn settle_metas(&self, k: usize) -> Vec<AccountMeta> {
        // the top-k of the payout order, as the site would pass them
        let v = vault_state(&self.svm, &self.vault);
        let mut entries: Vec<_> = v.heap[..v.heap_len as usize].to_vec();
        entries.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.idx.cmp(&b.idx)));
        entries.iter().take(k).flat_map(|e| { let o = self.owners[&(e.idx as u64)].pubkey(); vec![AccountMeta::new(pos_key(&self.vault, &o), false), AccountMeta::new(o, false)] }).collect()
    }
    fn buy_as(&mut self, who: &Keypair, min: u64, settle_k: usize) -> Result<(), String> {
        let mut metas = tenx_spiral::accounts::Buy { buyer: who.pubkey(), vault: self.vault, position: pos_key(&self.vault, &who.pubkey()), burn_buyer: bb(&self.vault), bonding_curve: self.curve, system_program: anchor_lang::system_program::ID }.to_account_metas(None);
        metas.extend(self.settle_metas(settle_k));
        let ix = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Buy { min_shares_out: min }.data(), metas);
        let payer = self.payer.insecure_clone();
        send(&mut self.svm, &[ix], &payer, &[who])
    }
    fn new_buyer(&mut self) -> Result<Keypair, String> {
        let k = Keypair::new();
        self.svm.airdrop(&k.pubkey(), SOL).unwrap();
        let idx = vault_state(&self.svm, &self.vault).positions_len;
        self.owners.insert(idx, k.insecure_clone());
        self.buy_as(&k, 0, 10)?;
        Ok(k)
    }
    fn exit(&mut self, who: &Keypair, min: u64) -> Result<(), String> {
        let ix = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Exit { min_native_out: min }.data(),
            tenx_spiral::accounts::Exit { owner: who.pubkey(), vault: self.vault, position: pos_key(&self.vault, &who.pubkey()) }.to_account_metas(None));
        let payer = self.payer.insecure_clone();
        send(&mut self.svm, &[ix], &payer, &[who])
    }
    fn solvent(&self) {
        let a = self.svm.get_account(&self.vault).unwrap();
        let v = vault_state(&self.svm, &self.vault);
        let need = self.svm.minimum_balance_for_rent_exemption(a.data.len()) + v.real_native;
        assert!(a.lamports >= need, "insolvent: {} < {}", a.lamports, need);
        let b = self.svm.get_account(&bb(&self.vault)).map(|x| x.lamports).unwrap_or(0);
        assert!(b >= v.burn_reserve + self.svm.minimum_balance_for_rent_exemption(0), "burn reserve not backed: {} < {}", b, v.burn_reserve);
        assert_eq!(v.total_in, v.total_out + v.real_native + v.burn_reserve + v.total_burned_native, "money in != out + pool + reserve + burned");
    }
}

/// Reference: the EVM vault's integer formulas, recomputed off-chain.
fn ref_quote_buy(rn: u64, rt: u64, x: u64) -> u64 { let k = rn as u128 * rt as u128; let d = rn as u128 + x as u128; (rt as u128 - (k + d - 1) / d) as u64 }
fn ref_quote_sell(rn: u64, rt: u64, real: u64, t: u64) -> u64 { let k = rn as u128 * rt as u128; let d = rt as u128 + t as u128; ((rn as u128 - (k + d - 1) / d) as u64).min(real) }

#[test]
fn init_rejects_bad_params() {
    for (name, a) in [
        ("ticket 0", InitArgs { ticket: 0, ..args() }),
        ("mult 1", InitArgs { target_mult: 1, ..args() }),
        ("vnative 0", InitArgs { virtual_native: 0, ..args() }),
        ("vtoken 0", InitArgs { virtual_token: 0, ..args() }),
        ("settle 0", InitArgs { max_settle_per_buy: 0, ..args() }),
        ("settle 17", InitArgs { max_settle_per_buy: 17, ..args() }),
        ("burn 3001", InitArgs { burn_bps: 3001, ..args() }),
        ("lock -1", InitArgs { lock_period: -1, ..args() }),
    ] { assert!(setup_with(a, false).is_err(), "accepted {name}"); }
    assert!(setup_with(args(), true).is_err(), "accepted a graduated curve");
    // wrong curve address / owner / quote mint
    let mut h = setup();
    let other = Keypair::new();
    let space = 8 + std::mem::size_of::<Vault>();
    let rent = h.svm.minimum_balance_for_rent_exemption(space);
    for (label, curve, owner, data) in [
        ("curve of another mint", Pubkey::find_program_address(&[pump::BONDING_CURVE_SEED, Pubkey::new_unique().as_ref()], &pump::PROGRAM_ID).0, pump::PROGRAM_ID, curve_bytes(false, Pubkey::default())),
        ("curve not owned by pump", h.curve, Pubkey::new_unique(), curve_bytes(false, Pubkey::default())),
        ("USDC-paired curve", h.curve, pump::PROGRAM_ID, curve_bytes(false, Pubkey::new_unique())),
    ] {
        put(&mut h.svm, curve, owner, data);
        let vk = Keypair::new();
        let create = solana_system_interface::instruction::create_account(&other.pubkey(), &vk.pubkey(), rent, space as u64, &tenx_spiral::ID);
        h.svm.airdrop(&other.pubkey(), 10 * SOL).unwrap();
        let init = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Initialize { a: args() }.data(),
            tenx_spiral::accounts::Initialize { creator: other.pubkey(), vault: vk.pubkey(), burn_buyer: bb(&vk.pubkey()), mint: h.mint, bonding_curve: curve, system_program: anchor_lang::system_program::ID }.to_account_metas(None));
        assert!(send(&mut h.svm, &[create, init], &other, &[&vk]).is_err(), "accepted {label}");
    }
    println!("PASS init_rejects_bad_params");
}

#[test]
fn shares_match_evm_formula_and_one_ticket_per_wallet() {
    let mut h = setup();
    let mut last = u64::MAX;
    for i in 0..12 {
        let before = vault_state(&h.svm, &h.vault);
        let who = h.new_buyer().unwrap();
        let after = vault_state(&h.svm, &h.vault);
        let p = position(&h.svm, &pos_key(&h.vault, &who.pubkey()));
        let to_burn = TICKET * 1500 / 10_000;
        assert_eq!(p.tokens, ref_quote_buy(before.reserve_native, before.reserve_token, TICKET - to_burn), "shares #{i}");
        assert_eq!(after.burn_reserve - before.burn_reserve, to_burn);
        assert_eq!(p.status, STATUS_LIVE);
        assert!(p.tokens < last, "later tickets must get fewer shares"); last = p.tokens;
        assert!(h.buy_as(&who, 0, 0).is_err(), "second ticket for the same wallet");
        h.solvent();
    }
    // slippage minimum
    let k = Keypair::new(); h.svm.airdrop(&k.pubkey(), SOL).unwrap();
    assert!(h.buy_as(&k, u64::MAX, 0).is_err(), "min_shares_out ignored");
    println!("PASS shares_match_evm_formula_and_one_ticket_per_wallet");
}

#[test]
fn payouts_go_most_shares_first_at_10x() {
    let mut h = setup();
    let mut paid_order = vec![];
    for _ in 0..140 {
        let pre: HashMap<u64, u64> = h.owners.iter().map(|(i, k)| (*i, h.svm.get_account(&k.pubkey()).unwrap().lamports)).collect();
        h.new_buyer().unwrap();
        h.solvent();
        for (i, k) in h.owners.iter() {
            let p = position(&h.svm, &pos_key(&h.vault, &k.pubkey()));
            if p.status == STATUS_AUTO_SOLD && !paid_order.contains(i) {
                assert!(p.received >= TICKET * 10, "paid below 10x");
                let got = h.svm.get_account(&k.pubkey()).unwrap().lamports - pre.get(i).copied().unwrap_or(0);
                assert_eq!(got, p.received, "owner did not receive the payout");
                paid_order.push(*i);
            }
        }
    }
    assert!(!paid_order.is_empty(), "nobody reached 10x in 140 tickets");
    // order invariant: every paid position holds at least as many shares as every position still waiting
    let (mut min_paid, mut max_live) = (u64::MAX, 0u64);
    for k in h.owners.values() { let p = position(&h.svm, &pos_key(&h.vault, &k.pubkey())); if p.status == STATUS_AUTO_SOLD { min_paid = min_paid.min(p.tokens) } else if p.status == STATUS_LIVE { max_live = max_live.max(p.tokens) } }
    assert!(min_paid >= max_live, "a waiting position has more shares than a paid one: {} < {}", min_paid, max_live);
    assert!(paid_order.contains(&0), "the first (largest) ticket was not paid");
    let v = vault_state(&h.svm, &h.vault);
    assert_eq!(v.auto_sold_count as usize, paid_order.len());
    println!("PASS payouts_go_most_shares_first_at_10x paid={} of 140: {:?}", paid_order.len(), paid_order);
}

#[test]
fn settlement_stops_without_accounts_and_settle_catches_up() {
    let mut h = setup();
    for _ in 0..60 {
        let k = Keypair::new(); h.svm.airdrop(&k.pubkey(), SOL).unwrap();
        let idx = vault_state(&h.svm, &h.vault).positions_len; h.owners.insert(idx, k.insecure_clone());
        h.buy_as(&k, 0, 0).unwrap(); // never pass settlement accounts
    }
    let v = vault_state(&h.svm, &h.vault);
    assert_eq!(v.auto_sold_count, 0);
    let top = v.heap[0];
    assert!(ref_quote_sell(v.reserve_native, v.reserve_token, v.real_native, top.tokens) >= TICKET * 10, "test needs a due position");
    // wrong owner account for the top position -> no payout
    let o = h.owners[&(top.idx as u64)].pubkey();
    let bad = vec![AccountMeta::new(pos_key(&h.vault, &o), false), AccountMeta::new(Pubkey::new_unique(), false)];
    let mut metas = tenx_spiral::accounts::Settle { vault: h.vault }.to_account_metas(None); metas.extend(bad);
    let ix = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Settle { max: 5 }.data(), metas);
    let payer = h.payer.insecure_clone();
    send(&mut h.svm, &[ix], &payer, &[]).unwrap();
    assert_eq!(vault_state(&h.svm, &h.vault).auto_sold_count, 0, "paid to a wrong owner account");
    let mut metas = tenx_spiral::accounts::Settle { vault: h.vault }.to_account_metas(None); metas.extend(h.settle_metas(10));
    let ix = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Settle { max: 3 }.data(), metas);
    send(&mut h.svm, &[ix], &payer, &[]).unwrap();
    let v2 = vault_state(&h.svm, &h.vault);
    assert!(v2.auto_sold_count >= 1 && v2.auto_sold_count <= 3, "settle max not honoured: {}", v2.auto_sold_count);
    assert_eq!(position(&h.svm, &pos_key(&h.vault, &o)).status, STATUS_AUTO_SOLD);
    h.solvent();
    println!("PASS settlement_stops_without_accounts_and_settle_catches_up settled={}", v2.auto_sold_count);
}

#[test]
fn exit_pays_curve_price_and_cannot_rebuy() {
    let mut h = setup();
    let mut ks = vec![];
    for _ in 0..8 { ks.push(h.new_buyer().unwrap()); }
    let who = &ks[3];
    let v = vault_state(&h.svm, &h.vault);
    let p = position(&h.svm, &pos_key(&h.vault, &who.pubkey()));
    let expect = ref_quote_sell(v.reserve_native, v.reserve_token, v.real_native, p.tokens);
    assert!(h.exit(who, expect + 1).is_err(), "exit min ignored");
    let bal = h.svm.get_account(&who.pubkey()).unwrap().lamports;
    h.exit(who, expect).unwrap();
    let p2 = position(&h.svm, &pos_key(&h.vault, &who.pubkey()));
    assert_eq!(p2.status, STATUS_EXITED); assert_eq!(p2.received, expect);
    assert_eq!(h.svm.get_account(&who.pubkey()).unwrap().lamports - bal, expect, "exit payout wrong (fees are paid by the payer account)");
    let v2 = vault_state(&h.svm, &h.vault);
    assert_eq!(v2.heap_len, 7); assert!(v2.heap[..7].iter().all(|e| e.idx != 3));
    assert!(h.exit(who, 0).is_err(), "double exit"); assert!(h.buy_as(who, 0, 0).is_err(), "rebuy after exit");
    // heap order intact after removal from the middle
    let mut e: Vec<_> = v2.heap[..7].to_vec(); e.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.idx.cmp(&b.idx)));
    assert_eq!(v2.heap[0].idx, e[0].idx);
    h.solvent();
    println!("PASS exit_pays_curve_price_and_cannot_rebuy");
}

#[test]
fn lock_period_blocks_early_exit() {
    let mut h = setup_with(InitArgs { lock_period: 3600, ..args() }, false).unwrap();
    let who = h.new_buyer().unwrap();
    assert!(h.exit(&who, 0).is_err(), "exited inside the lock period");
    let mut c: solana_clock::Clock = h.svm.get_sysvar();
    c.unix_timestamp += 3601; h.svm.set_sysvar(&c);
    h.exit(&who, 0).unwrap();
    println!("PASS lock_period_blocks_early_exit");
}

#[test]
fn graduation_stops_burn_split_and_releases_reserve() {
    let mut h = setup();
    for _ in 0..5 { h.new_buyer().unwrap(); }
    let v = vault_state(&h.svm, &h.vault);
    assert_eq!(v.burn_reserve, 5 * TICKET * 1500 / 10_000);
    let rel = |h: &mut H| { let ix = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::ReleaseBurnReserve {}.data(),
        tenx_spiral::accounts::Release { vault: h.vault, bonding_curve: h.curve, burn_buyer: bb(&h.vault), system_program: anchor_lang::system_program::ID }.to_account_metas(None));
        let payer = h.payer.insecure_clone(); send(&mut h.svm, &[ix], &payer, &[]) };
    assert!(rel(&mut h).is_err(), "released while still on the curve");
    // stale valve: 29 days without a burn -> still refused; 30 days -> allowed (then re-arm by restoring the curve test below)
    let mut c: solana_clock::Clock = h.svm.get_sysvar(); let t0 = c.unix_timestamp;
    c.unix_timestamp = t0 + 29 * 24 * 3600; h.svm.set_sysvar(&c);
    assert!(rel(&mut h).is_err(), "released before 30 days without a burn");
    c.unix_timestamp = t0; h.svm.set_sysvar(&c);
    let curve = h.curve; put(&mut h.svm, curve, pump::PROGRAM_ID, curve_bytes(true, Pubkey::default()));
    let before = vault_state(&h.svm, &h.vault);
    rel(&mut h).unwrap();
    let after = vault_state(&h.svm, &h.vault);
    assert_eq!(after.burn_reserve, 0); assert_eq!(after.real_native, before.real_native + before.burn_reserve);
    assert_eq!(after.burn_released_native, before.burn_reserve);
    let b2 = vault_state(&h.svm, &h.vault); h.new_buyer().unwrap(); let a2 = vault_state(&h.svm, &h.vault);
    assert_eq!(a2.burn_reserve, 0, "graduated coin still split"); assert_eq!(a2.real_native - b2.real_native, TICKET);
    h.solvent();
    println!("PASS graduation_stops_burn_split_and_releases_reserve");
}

#[test]
fn burn_buy_refuses_bad_accounts_before_pump() {
    let mut h = setup();
    h.new_buyer().unwrap();
    let buyer = Pubkey::find_program_address(&[BUYER_SEED, h.vault.as_ref()], &tenx_spiral::ID).0;
    let tp = anchor_spl::token::ID;
    let ata = anchor_spl::associated_token::get_associated_token_address_with_program_id(&buyer, &h.mint, &tp);
    let mk = |mint: Pubkey, curve: Pubkey, min: u64| Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::BurnBuy { min_tokens_out: min }.data(),
        tenx_spiral::accounts::BurnBuy { caller: h.payer.pubkey(), vault: h.vault, buyer, buyer_token: anchor_spl::associated_token::get_associated_token_address_with_program_id(&buyer, &mint, &tp),
            mint, bonding_curve: curve, associated_bonding_curve: Pubkey::new_unique(), pump_global: Pubkey::new_unique(), fee_recipient: Pubkey::new_unique(),
            creator_vault: Pubkey::new_unique(), event_authority: Pubkey::new_unique(), pump_program: pump::PROGRAM_ID, global_volume_accumulator: Pubkey::new_unique(),
            user_volume_accumulator: Pubkey::new_unique(), fee_config: Pubkey::new_unique(), fee_program: pump::FEE_PROGRAM_ID, token_program: tp,
            associated_token_program: anchor_spl::associated_token::ID, system_program: anchor_lang::system_program::ID }.to_account_metas(None));
    let payer = h.payer.insecure_clone();
    let other_mint = Pubkey::new_unique(); put(&mut h.svm, other_mint, tp, mint_bytes());
    assert!(send(&mut h.svm, &[mk(other_mint, h.curve, 1)], &payer, &[]).unwrap_err().contains("AccountMismatch"), "other mint");
    assert!(send(&mut h.svm, &[mk(h.mint, Pubkey::new_unique(), 1)], &payer, &[]).unwrap_err().contains("AccountMismatch"), "other curve");
    assert!(send(&mut h.svm, &[mk(h.mint, h.curve, 1)], &payer, &[]).unwrap_err().contains("BurnMinTooLow"), "min 1 accepted");
    let _ = ata;
    let v = vault_state(&h.svm, &h.vault); assert_eq!(v.burn_reserve, TICKET * 1500 / 10_000, "reserve moved on a refused call");
    println!("PASS burn_buy_refuses_bad_accounts_before_pump");
}

#[test]
fn stale_burns_release_after_30_days() {
    let mut h = setup();
    for _ in 0..3 { h.new_buyer().unwrap(); }
    let r0 = vault_state(&h.svm, &h.vault);
    let mut c: solana_clock::Clock = h.svm.get_sysvar();
    c.unix_timestamp += 30 * 24 * 3600 + 1; h.svm.set_sysvar(&c);
    let ix = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::ReleaseBurnReserve {}.data(),
        tenx_spiral::accounts::Release { vault: h.vault, bonding_curve: h.curve, burn_buyer: bb(&h.vault), system_program: anchor_lang::system_program::ID }.to_account_metas(None));
    let payer = h.payer.insecure_clone(); send(&mut h.svm, &[ix], &payer, &[]).unwrap();
    let r1 = vault_state(&h.svm, &h.vault);
    assert_eq!(r1.burn_reserve, 0); assert_eq!(r1.real_native, r0.real_native + r0.burn_reserve);
    h.solvent();
    println!("PASS stale_burns_release_after_30_days");
}

#[test]
fn vault_full_at_cap_then_frees_on_exit() {
    let mut h = setup_with(InitArgs { target_mult: 1000, ..args() }, false).unwrap(); // no payouts: fill the heap
    let cap = tenx_spiral::constants::HEAP_CAP;
    let mut first = None;
    for i in 0..cap { let k = h.new_buyer().unwrap(); if i == 0 { first = Some(k); } }
    assert_eq!(vault_state(&h.svm, &h.vault).heap_len as usize, cap);
    let extra = Keypair::new(); h.svm.airdrop(&extra.pubkey(), SOL).unwrap();
    let e = h.buy_as(&extra, 0, 0).unwrap_err();
    assert!(e.contains("VaultFull"), "expected VaultFull, got {}", &e[..e.len().min(200)]);
    h.exit(&first.unwrap(), 0).unwrap();
    h.buy_as(&extra, 0, 0).unwrap();
    assert_eq!(vault_state(&h.svm, &h.vault).heap_len as usize, cap);
    h.solvent();
    println!("PASS vault_full_at_cap_then_frees_on_exit cap={cap}");
}

#[test]
fn heap_matches_reference_under_random_exits() {
    let mut h = setup_with(InitArgs { target_mult: 1000, ..args() }, false).unwrap(); // no payouts: pure ordering
    let mut ks = vec![];
    let mut seed = 0x9e3779b97f4a7c15u64;
    let mut rnd = || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed };
    for round in 0..120 {
        if ks.len() > 5 && rnd() % 3 == 0 {
            let i = (rnd() as usize) % ks.len();
            let k: Keypair = ks.remove(i);
            h.exit(&k, 0).unwrap();
        } else { ks.push(h.new_buyer().unwrap()); }
        let v = vault_state(&h.svm, &h.vault);
        let mut live: Vec<(u64, u64)> = ks.iter().map(|k| { let p = position(&h.svm, &pos_key(&h.vault, &k.pubkey())); (p.tokens, p.idx) }).collect();
        live.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        assert_eq!(v.heap_len as usize, live.len(), "round {round}");
        assert_eq!((v.heap[0].tokens, v.heap[0].idx as u64), live[0], "heap top wrong at round {round}");
        for s in 1..v.heap_len as usize { let p = (s - 1) / 2; assert!(!v.heap[s].above(&v.heap[p]), "heap property broken at {s} round {round}"); }
        h.solvent();
    }
    println!("PASS heap_matches_reference_under_random_exits");
}
