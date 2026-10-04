//! Standard vaults: anyone opens one per coin per tier at [VAULT_SEED, mint, tier] and pays its (small) rent;
//! an unused vault can be closed by anyone and the rent goes back only to whoever opened it.
use {
    anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction, InstructionData, ToAccountMetas},
    litesvm::LiteSVM,
    solana_account::Account,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    tenx_spiral::{constants::{pump, BUYER_SEED, POS_SEED, TIER_TICKETS, VAULT_SEED, STD_BURN_BPS, STD_TARGET_MULT}, state::Vault},
};

const SOL: u64 = 1_000_000_000;

fn program_bytes() -> Vec<u8> { std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/tenx_spiral.so")).unwrap() }
fn bb(vault: &Pubkey) -> Pubkey { Pubkey::find_program_address(&[BUYER_SEED, vault.as_ref()], &tenx_spiral::ID).0 }
fn vpda(mint: &Pubkey, tier: u8) -> Pubkey { Pubkey::find_program_address(&[VAULT_SEED, mint.as_ref(), &[tier]], &tenx_spiral::ID).0 }
fn lamports(svm: &LiteSVM, k: &Pubkey) -> u64 { svm.get_account(k).map(|a| a.lamports).unwrap_or(0) }
fn put(svm: &mut LiteSVM, key: Pubkey, owner: Pubkey, data: Vec<u8>) {
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(key, Account { lamports, data, owner, executable: false, rent_epoch: 0 }).unwrap();
}
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
fn vault_state(svm: &LiteSVM, vault: &Pubkey) -> Vault {
    let a = svm.get_account(vault).unwrap();
    *bytemuck::from_bytes::<Vault>(&a.data[8..8 + std::mem::size_of::<Vault>()])
}

struct H { svm: LiteSVM, payer: Keypair, mint: Pubkey, curve: Pubkey }
fn setup(complete: bool) -> H {
    let mut svm = LiteSVM::new();
    svm.add_program(tenx_spiral::ID, &program_bytes()).unwrap();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 100 * SOL).unwrap();
    let mint = Pubkey::new_unique();
    let mut m = vec![0u8; 82]; m[44] = 6; m[45] = 1;
    put(&mut svm, mint, anchor_spl::token::ID, m);
    let curve = Pubkey::find_program_address(&[pump::BONDING_CURVE_SEED, mint.as_ref()], &pump::PROGRAM_ID).0;
    let mut d = vec![0u8; 151];
    d[..8].copy_from_slice(&pump::BONDING_CURVE_DISC);
    d[8..16].copy_from_slice(&1_073_000_000_000_000u64.to_le_bytes());
    d[16..24].copy_from_slice(&30_000_000_000u64.to_le_bytes());
    d[48] = complete as u8;
    put(&mut svm, curve, pump::PROGRAM_ID, d);
    H { svm, payer, mint, curve }
}
fn open_ix(payer: Pubkey, mint: Pubkey, curve: Pubkey, tier: u8) -> Instruction {
    let vault = vpda(&mint, tier);
    Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::OpenVault { tier }.data(),
        tenx_spiral::accounts::OpenVault { payer, vault, burn_buyer: bb(&vault), mint, bonding_curve: curve, system_program: anchor_lang::system_program::ID }.to_account_metas(None))
}
fn buy_ix(buyer: Pubkey, vault: Pubkey, curve: Pubkey) -> Instruction {
    let pos = Pubkey::find_program_address(&[POS_SEED, vault.as_ref(), buyer.as_ref()], &tenx_spiral::ID).0;
    Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::Buy { min_shares_out: 0 }.data(),
        tenx_spiral::accounts::Buy { buyer, vault, position: pos, burn_buyer: bb(&vault), bonding_curve: curve, system_program: anchor_lang::system_program::ID }.to_account_metas(None))
}
fn close_ix(vault: Pubkey, creator: Pubkey) -> Instruction {
    Instruction::new_with_bytes(tenx_spiral::ID, &tenx_spiral::instruction::CloseEmptyVault {}.data(),
        tenx_spiral::accounts::CloseEmpty { vault, burn_buyer: bb(&vault), creator, system_program: anchor_lang::system_program::ID }.to_account_metas(None))
}

#[test]
fn open_vault_is_standard_small_and_at_the_pda() {
    let mut h = setup(false);
    let before = lamports(&h.svm, &h.payer.pubkey());
    let (p, m, c) = (h.payer.pubkey(), h.mint, h.curve);
    let payer = h.payer.insecure_clone();
    send(&mut h.svm, &[open_ix(p, m, c, 0)], &payer, &[]).unwrap();
    let vault = vpda(&m, 0);
    let a = h.svm.get_account(&vault).unwrap();
    let v = vault_state(&h.svm, &vault);
    assert_eq!(v.ticket, TIER_TICKETS[0]);
    assert_eq!(v.target_mult, STD_TARGET_MULT);
    assert_eq!(v.burn_bps, STD_BURN_BPS);
    assert_eq!(v.virtual_native, 2 * TIER_TICKETS[0]);
    assert_eq!(v.creator, p);
    assert_eq!(v.mint, m);
    let cost = before - lamports(&h.svm, &p);
    println!("vault account {} bytes, opener paid {} lamports ({:.5} SOL incl. buyer float + fee)", a.data.len(), cost, cost as f64 / 1e9);
    assert!(a.data.len() <= 10_240, "vault account must fit one instruction's create: {}", a.data.len());
    assert!(cost < 70_000_000, "opening costs too much: {cost}");
}

#[test]
fn one_vault_per_coin_per_tier_and_only_known_tiers() {
    let mut h = setup(false);
    let (p, m, c) = (h.payer.pubkey(), h.mint, h.curve);
    let payer = h.payer.insecure_clone();
    send(&mut h.svm, &[open_ix(p, m, c, 1)], &payer, &[]).unwrap();
    assert!(send(&mut h.svm, &[open_ix(p, m, c, 1)], &payer, &[]).is_err(), "second open of the same tier must fail");
    assert!(send(&mut h.svm, &[open_ix(p, m, c, 2)], &payer, &[]).is_err(), "unknown tier must fail");
    send(&mut h.svm, &[open_ix(p, m, c, 0)], &payer, &[]).unwrap();
    assert_eq!(vault_state(&h.svm, &vpda(&m, 1)).ticket, TIER_TICKETS[1]);
}

#[test]
fn first_buyer_opens_and_buys_in_one_transaction() {
    let mut h = setup(false);
    let (m, c) = (h.mint, h.curve);
    let buyer = Keypair::new();
    h.svm.airdrop(&buyer.pubkey(), SOL).unwrap();
    let vault = vpda(&m, 1);
    send(&mut h.svm, &[open_ix(buyer.pubkey(), m, c, 1), buy_ix(buyer.pubkey(), vault, c)], &buyer, &[]).unwrap();
    let v = vault_state(&h.svm, &vault);
    assert_eq!(v.positions_len, 1);
    assert_eq!(v.total_in, TIER_TICKETS[1]);
    assert_eq!(v.creator, buyer.pubkey());
}

#[test]
fn close_empty_refunds_only_the_opener_and_never_a_used_vault() {
    let mut h = setup(false);
    let (m, c) = (h.mint, h.curve);
    let opener = Keypair::new();
    h.svm.airdrop(&opener.pubkey(), SOL).unwrap();
    let stranger = Keypair::new();
    h.svm.airdrop(&stranger.pubkey(), SOL).unwrap();
    send(&mut h.svm, &[open_ix(opener.pubkey(), m, c, 0)], &opener, &[]).unwrap();
    let vault = vpda(&m, 0);
    let want = lamports(&h.svm, &vault) + lamports(&h.svm, &bb(&vault));
    // someone else tries to send the rent to themselves
    assert!(send(&mut h.svm, &[close_ix(vault, stranger.pubkey())], &stranger, &[]).is_err());
    // anyone may close it, but the rent goes to the opener
    let before = lamports(&h.svm, &opener.pubkey());
    send(&mut h.svm, &[close_ix(vault, opener.pubkey())], &stranger, &[]).unwrap();
    assert_eq!(lamports(&h.svm, &vault), 0);
    assert_eq!(lamports(&h.svm, &opener.pubkey()) - before, want);
    // reopen, sell one ticket: now it can never be closed
    send(&mut h.svm, &[open_ix(opener.pubkey(), m, c, 0)], &opener, &[]).unwrap();
    let buyer = Keypair::new();
    h.svm.airdrop(&buyer.pubkey(), SOL).unwrap();
    send(&mut h.svm, &[buy_ix(buyer.pubkey(), vault, c)], &buyer, &[]).unwrap();
    let e = send(&mut h.svm, &[close_ix(vault, opener.pubkey())], &opener, &[]).unwrap_err();
    assert!(e.contains("NotEmpty") || e.contains("6016"), "{e}");
}

#[test]
fn graduated_coin_cannot_get_a_vault() {
    let mut h = setup(true);
    let (p, m, c) = (h.payer.pubkey(), h.mint, h.curve);
    let payer = h.payer.insecure_clone();
    let e = send(&mut h.svm, &[open_ix(p, m, c, 0)], &payer, &[]).unwrap_err();
    assert!(e.contains("Graduated"), "{e}");
}
