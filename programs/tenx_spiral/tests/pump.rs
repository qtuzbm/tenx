//! burn_buy against the real pump.fun programs and a real coin (accounts dumped from mainnet into fixtures/).
use {
    anchor_lang::{prelude::Pubkey, solana_program::instruction::{AccountMeta, Instruction}, InstructionData, ToAccountMetas},
    base64::Engine,
    litesvm::LiteSVM,
    solana_account::Account,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::str::FromStr,
    tenx_spiral::{constants::{pump, BUYER_SEED, POS_SEED}, state::Vault, InitArgs},
};

const SOL: u64 = 1_000_000_000;
const TICKET: u64 = SOL / 10;
const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
fn pk(s: &str) -> Pubkey { Pubkey::from_str(s).unwrap() }
#[derive(Clone, Copy)]
struct Coin { name: &'static str, mint: &'static str, curve: &'static str, acurve: &'static str, creator_vault: &'static str, fee_recipient: &'static str }
// mayhem-mode coin: fee recipient from global.reserved_fee_recipient(s)
const JASON: Coin = Coin { name: "mayhem", mint: "8fcSG3JFZczpxYvCnuySMgVNwFV9Bxk8m2LVCEULpump", curve: "9S3DJBd3Sk4xDoQ5KmDv4bErZ6XtE462wSjyrRwB2A26",
    acurve: "CamG5goomu9Jj7kNcH76QD869ETVYbHm2ticJhz7nDDq", creator_vault: "CDYG2bBu3UkhALJwZp7pSSmmzPrhMb4ysxNWwPYGJTmA", fee_recipient: "GesfTA3X2arioaHp8bbKdjG9vJtskViWACZoYvxp4twS" };
// normal coin: fee recipient from global.fee_recipient(s)
const NICKELSON: Coin = Coin { name: "normal", mint: "FzUUqXNapxnxkq8j8C5B3h6eoJ6XQefQoZVUyppUpump", curve: "J8sFKfLmhAmekNkrUCHy4G1jRgMKn6kk7rrtuURFXDJN",
    acurve: "9hiF8hkXsSWUT6EYAK293C8KaHtPkH987bxzHR1oY4Q8", creator_vault: "cgjjcd3GSMSAAMikh6FonyRXXdsLLZp43Ae5pNe19Gs", fee_recipient: "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV" };
const GLOBAL: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";
const EVENT_AUTH: &str = "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1";
const GVA: &str = "Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y";
const FEE_CONFIG: &str = "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt";
const BUYBACK_FEE: &str = "GXPFM2caqTtQYC2cJ5yJRi9VDkpsYZXzYdwYpGnLmtDL";

fn load(svm: &mut LiteSVM, key: &str) {
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{FIX}/acc_{key}.json")).unwrap()).unwrap();
    let a = &j["account"];
    let data = base64::engine::general_purpose::STANDARD.decode(a["data"][0].as_str().unwrap()).unwrap();
    svm.set_account(pk(key), Account { lamports: a["lamports"].as_u64().unwrap(), data, owner: pk(a["owner"].as_str().unwrap()), executable: false, rent_epoch: 0 }).unwrap();
}
fn bb(vault: &Pubkey) -> Pubkey { Pubkey::find_program_address(&[BUYER_SEED, vault.as_ref()], &tenx_spiral::ID).0 }
fn send(svm: &mut LiteSVM, ixs: &[Instruction], payer: &Keypair, extra: &[&Keypair]) -> Result<Vec<String>, String> {
    let bh = svm.latest_blockhash();
    let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &bh);
    let mut signers: Vec<&Keypair> = vec![payer]; signers.extend_from_slice(extra);
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers).map_err(|e| e.to_string())?;
    let r = svm.send_transaction(tx).map(|m| m.logs).map_err(|e| format!("{:?} | {}", e.err, e.meta.logs.join(" / ")));
    svm.expire_blockhash();
    r
}
fn vault_state(svm: &LiteSVM, vault: &Pubkey) -> Vault { let a = svm.get_account(vault).unwrap(); *bytemuck::from_bytes::<Vault>(&a.data[8..8 + std::mem::size_of::<Vault>()]) }
fn u64_at(svm: &LiteSVM, k: &Pubkey, o: usize) -> u64 { let a = svm.get_account(k).unwrap(); u64::from_le_bytes(a.data[o..o + 8].try_into().unwrap()) }

struct P { svm: LiteSVM, payer: Keypair, vault: Pubkey, buyer: Pubkey, buyer_token: Pubkey, c: Coin }

fn setup(c: Coin, n_tickets: usize) -> P {
    let mut svm = LiteSVM::new();
    svm.add_program(tenx_spiral::ID, &std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/tenx_spiral.so")).unwrap()).unwrap();
    svm.add_program(pump::PROGRAM_ID, &std::fs::read(format!("{FIX}/pump.so")).unwrap()).unwrap();
    svm.add_program(pump::FEE_PROGRAM_ID, &std::fs::read(format!("{FIX}/pump_fees.so")).unwrap()).unwrap();
    for k in [GLOBAL, c.fee_recipient, c.mint, c.curve, c.acurve, GVA, FEE_CONFIG, BUYBACK_FEE] { load(&mut svm, k); }
    if std::path::Path::new(&format!("{FIX}/acc_{}.json", c.creator_vault)).exists() { load(&mut svm, c.creator_vault); }
    let payer = Keypair::new(); svm.airdrop(&payer.pubkey(), 100 * SOL).unwrap();
    let vk = Keypair::new();
    let space = 8 + std::mem::size_of::<Vault>();
    let rent = svm.minimum_balance_for_rent_exemption(space);
    let a = InitArgs { ticket: TICKET, target_mult: 10, lock_period: 0, virtual_native: 2 * TICKET, virtual_token: 1_000_000_000_000_000, max_settle_per_buy: 10, burn_bps: 1500 };
    let create = solana_system_interface::instruction::create_account(&payer.pubkey(), &vk.pubkey(), rent, space as u64, &tenx_spiral::ID);
    let init = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Initialize { a }.data(),
        tenx_spiral::accounts::Initialize { creator: payer.pubkey(), vault: vk.pubkey(), burn_buyer: bb(&vk.pubkey()), mint: pk(c.mint), bonding_curve: pk(c.curve), system_program: anchor_lang::system_program::ID }.to_account_metas(None));
    send(&mut svm, &[create, init], &payer, &[&vk]).unwrap();
    let vault = vk.pubkey();
    for _ in 0..n_tickets {
        let b = Keypair::new(); svm.airdrop(&b.pubkey(), SOL).unwrap();
        let pos = Pubkey::find_program_address(&[POS_SEED, vault.as_ref(), b.pubkey().as_ref()], &tenx_spiral::ID).0;
        let ix = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Buy { min_shares_out: 0 }.data(),
            tenx_spiral::accounts::Buy { buyer: b.pubkey(), vault, position: pos, burn_buyer: bb(&vault), bonding_curve: pk(c.curve), system_program: anchor_lang::system_program::ID }.to_account_metas(None));
        send(&mut svm, &[ix], &payer, &[&b]).unwrap();
    }
    let buyer = Pubkey::find_program_address(&[BUYER_SEED, vault.as_ref()], &tenx_spiral::ID).0;
    let buyer_token = anchor_spl::associated_token::get_associated_token_address_with_program_id(&buyer, &pk(c.mint), &anchor_spl::token_2022::ID);
    P { svm, payer, vault, buyer, buyer_token, c }
}

impl P {
    fn burn_ix(&self, min: u64, trailing: bool) -> Instruction {
        let uva = Pubkey::find_program_address(&[b"user_volume_accumulator", self.buyer.as_ref()], &pump::PROGRAM_ID).0;
        let mut metas = tenx_spiral::accounts::BurnBuy { caller: self.payer.pubkey(), vault: self.vault, buyer: self.buyer, buyer_token: self.buyer_token,
            mint: pk(self.c.mint), bonding_curve: pk(self.c.curve), associated_bonding_curve: pk(self.c.acurve), pump_global: pk(GLOBAL), fee_recipient: pk(self.c.fee_recipient),
            creator_vault: pk(self.c.creator_vault), event_authority: pk(EVENT_AUTH), pump_program: pump::PROGRAM_ID, global_volume_accumulator: pk(GVA),
            user_volume_accumulator: uva, fee_config: pk(FEE_CONFIG), fee_program: pump::FEE_PROGRAM_ID, token_program: anchor_spl::token_2022::ID,
            associated_token_program: anchor_spl::associated_token::ID, system_program: anchor_lang::system_program::ID }.to_account_metas(None);
        if trailing {
            // as @pump-fun/pump-sdk 2.0.0 getBuyInstructionRaw: bonding-curve-v2 (read-only), then a buyback fee recipient (writable)
            let v2 = Pubkey::find_program_address(&[b"bonding-curve-v2", pk(self.c.mint).as_ref()], &pump::PROGRAM_ID).0;
            metas.push(AccountMeta::new_readonly(v2, false));
            metas.push(AccountMeta::new(pk(BUYBACK_FEE), false));
        }
        Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::BurnBuy { min_tokens_out: min }.data(), metas)
    }
    fn burn(&mut self, min: u64, trailing: bool, extra: Vec<Instruction>, signers: &[&Keypair]) -> Result<Vec<String>, String> {
        let mut ixs = extra; ixs.push(self.burn_ix(min, trailing));
        let payer = self.payer.insecure_clone();
        send(&mut self.svm, &ixs, &payer, signers)
    }
    fn fee_free_quote(&self, amt: u64) -> u64 {
        let vt = u64_at(&self.svm, &pk(self.c.curve), 8) as u128; let vq = u64_at(&self.svm, &pk(self.c.curve), 16) as u128;
        (vt - (vt * vq).div_ceil(vq + amt as u128)) as u64
    }
}

#[test]
fn burn_buy_buys_on_the_real_curve_and_burns_everything() { for c in [JASON, NICKELSON] { burn_case(c); } }
fn burn_case(c: Coin) {
    let mut p = setup(c, 3);
    let v0 = vault_state(&p.svm, &p.vault);
    assert_eq!(v0.burn_reserve, 3 * TICKET * 1500 / 10_000);
    let supply0 = u64_at(&p.svm, &pk(c.mint), 36);
    let real_quote0 = u64_at(&p.svm, &pk(c.curve), 32);
    let q = p.fee_free_quote(v0.burn_reserve);
    let payer = p.payer.insecure_clone();

    // without the trailing accounts the current pump.fun program refuses -> whole tx reverts, reserve untouched
    let e = p.burn(q * 96 / 100, false, vec![], &[]).unwrap_err();
    assert_eq!(vault_state(&p.svm, &p.vault).burn_reserve, v0.burn_reserve, "reserve moved on a reverted burn");
    println!("[{}] without trailing accounts: refused ({})", c.name, e.chars().take(160).collect::<String>());
    // a minimum above what the curve can give -> reverts
    assert!(p.burn(q * 2, true, vec![], &[]).is_err(), "impossible minimum accepted");
    assert_eq!(vault_state(&p.svm, &p.vault).burn_reserve, v0.burn_reserve);

    let logs = p.burn(q * 96 / 100, true, vec![], &[]).unwrap();
    let v1 = vault_state(&p.svm, &p.vault);
    let supply1 = u64_at(&p.svm, &pk(c.mint), 36);
    assert_eq!(v1.burn_reserve, 0);
    assert_eq!(v1.burn_count, 1);
    assert_eq!(v1.total_burned_native, v0.burn_reserve);
    assert!(v1.total_burned_tokens >= q * 96 / 100, "bought too little: {} vs quote {}", v1.total_burned_tokens, q);
    assert_eq!(supply0 - supply1, v1.total_burned_tokens, "mint supply did not drop by the burned amount");
    assert_eq!(u64_at(&p.svm, &p.buyer_token, 64), 0, "tokens left in the buyer account");
    assert!(u64_at(&p.svm, &pk(c.curve), 32) > real_quote0, "curve did not receive SOL");
    let a = p.svm.get_account(&p.vault).unwrap();
    assert!(a.lamports >= p.svm.minimum_balance_for_rent_exemption(a.data.len()) + v1.real_native, "vault insolvent after burn");
    println!("PASS [{}] burn_buy: spent {} lamports, bought+burned {} tokens (fee-free quote {}), supply {} -> {}, cu-log: {}",
        c.name, v1.total_burned_native, v1.total_burned_tokens, q, supply0, supply1, logs.iter().rev().find(|l| l.contains("consumed")).cloned().unwrap_or_default());

    // second round works with the accounts already created
    let b = Keypair::new(); p.svm.airdrop(&b.pubkey(), SOL).unwrap();
    let pos = Pubkey::find_program_address(&[POS_SEED, p.vault.as_ref(), b.pubkey().as_ref()], &tenx_spiral::ID).0;
    let ix = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Buy { min_shares_out: 0 }.data(),
        tenx_spiral::accounts::Buy { buyer: b.pubkey(), vault: p.vault, position: pos, burn_buyer: bb(&p.vault), bonding_curve: pk(c.curve), system_program: anchor_lang::system_program::ID }.to_account_metas(None));
    send(&mut p.svm, &[ix], &payer, &[&b]).unwrap();
    let r = vault_state(&p.svm, &p.vault).burn_reserve; let q2 = p.fee_free_quote(r);
    p.burn(q2 * 96 / 100, true, vec![], &[]).unwrap();
    let v2 = vault_state(&p.svm, &p.vault);
    assert_eq!(v2.burn_count, 2); assert_eq!(v2.burn_reserve, 0);
    assert_eq!(supply0 - u64_at(&p.svm, &pk(c.mint), 36), v2.total_burned_tokens);
    println!("PASS [{}] second burn_buy: total burned {} tokens for {} lamports", c.name, v2.total_burned_tokens, v2.total_burned_native);
}

#[test]
fn buy_ticket_and_burn_in_one_transaction() { for c in [JASON, NICKELSON] { one_tx_case(c); } }
fn one_tx_case(c: Coin) {
    // what the site sends: buy + burn_buy together; the burn uses the reserve including this ticket's share
    let mut p = setup(c, 0);
    let payer = p.payer.insecure_clone();
    let b = Keypair::new(); p.svm.airdrop(&b.pubkey(), SOL).unwrap();
    let pos = Pubkey::find_program_address(&[POS_SEED, p.vault.as_ref(), b.pubkey().as_ref()], &tenx_spiral::ID).0;
    let buy = Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Buy { min_shares_out: 0 }.data(),
        tenx_spiral::accounts::Buy { buyer: b.pubkey(), vault: p.vault, position: pos, burn_buyer: bb(&p.vault), bonding_curve: pk(c.curve), system_program: anchor_lang::system_program::ID }.to_account_metas(None));
    let q = p.fee_free_quote(TICKET * 1500 / 10_000);
    p.burn(q * 96 / 100, true, vec![buy], &[&b]).unwrap();
    let v = vault_state(&p.svm, &p.vault);
    assert_eq!(v.burn_count, 1); assert_eq!(v.burn_reserve, 0); assert_eq!(v.live_count, 1);
    println!("PASS [{}] buy+burn in one tx: burned {} tokens", c.name, v.total_burned_tokens);
}
