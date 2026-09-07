// Someone sends money to a person who has no Stellar account yet. The account
// is created, opens its trustline, takes the money, and pays for all of it out
// of that same money — in one transaction, without ever holding XLM.
//
//   npm run build
//   FUNDER_SECRET=S... RESERVE_URL=http://127.0.0.1:8080 node examples/zero-xlm-account.mjs
//
// FUNDER_SECRET is any funded testnet account; `stellar keys generate --fund`
// gives you one. It plays the part of the sender.
import {
  Asset,
  BASE_FEE,
  Claimant,
  Horizon,
  Keypair,
  Networks,
  Operation,
  TransactionBuilder,
} from "@stellar/stellar-sdk";
import { Reserve } from "../dist/index.js";

const url = process.env.RESERVE_URL ?? "http://127.0.0.1:8080";
const horizonUrl = process.env.HORIZON_URL ?? "https://horizon-testnet.stellar.org";
const horizon = new Horizon.Server(horizonUrl);
const reserve = new Reserve({ url, network: "testnet" });

const funder = Keypair.fromSecret(process.env.FUNDER_SECRET);
const user = Keypair.random();
console.log("sender:   ", funder.publicKey());
console.log("recipient:", user.publicKey(), "(no account yet)");

// The sender issues their own token here so the example needs nothing else.
const token = new Asset("DEMO", funder.publicKey());

async function submit(source, ops, extraSigners = []) {
  const account = await horizon.loadAccount(source.publicKey());
  const builder = new TransactionBuilder(account, {
    fee: BASE_FEE,
    networkPassphrase: Networks.TESTNET,
  });
  for (const op of ops) builder.addOperation(op);
  const tx = builder.setTimeout(120).build();
  tx.sign(source, ...extraSigners);
  return horizon.submitTransaction(tx);
}

// A fee token is only usable if it can be sold for XLM: that is how the fee
// actually settles. A brand new asset has no market, so the example makes one.
const maker = Keypair.random();
await fetch(`https://friendbot.stellar.org?addr=${maker.publicKey()}`);
await submit(maker, [Operation.changeTrust({ asset: token })]);
await submit(funder, [
  Operation.payment({ destination: maker.publicKey(), asset: token, amount: "100000" }),
]);
await submit(maker, [
  Operation.manageSellOffer({
    selling: Asset.native(),
    buying: token,
    amount: "5000",
    price: "0.5",
  }),
]);
console.log("market: 5000 XLM offered at 0.5 DEMO/XLM");

// Money can be left for an address that has no account: that is what a
// claimable balance is for.
await submit(funder, [
  Operation.createClaimableBalance({
    asset: token,
    amount: "20",
    claimants: [new Claimant(user.publicKey())],
  }),
]);

const [balance] = (await horizon.claimableBalances().claimant(user.publicKey()).call()).records;
console.log(`sent 20 DEMO, waiting as ${balance.id.slice(0, 16)}…`);

// Now the recipient collects it. Everything they need happens at once.
const quote = await reserve.quote({
  source: user.publicKey(),
  feeToken: `DEMO:${funder.publicKey()}`,
  // 20 DEMO is arriving; 10 is above reserves-plus-fees at the 0.5 DEMO/XLM
  // market this example just posted, and still a bound.
  maxSendStroops: 100_000_000,
  ops: [
    { type: "create_account", destination: user.publicKey() },
    { type: "change_trust", asset: `DEMO:${funder.publicKey()}` },
    { type: "claim_balance", balance_id: balance.id },
  ],
});
console.log(
  `creates the account: ${quote.createsAccount}, costs ${quote.sendMaxStroops} stroops of DEMO`,
);

const result = await reserve.send(quote, (xdr, { networkPassphrase }) => {
  const built = TransactionBuilder.fromXDR(xdr, networkPassphrase);
  built.sign(user);
  return built.toXDR();
});
console.log("submitted:", result.hash);

const created = await horizon.loadAccount(user.publicKey());
const xlm = created.balances.find((b) => b.asset_type === "native");
const demo = created.balances.find((b) => b.asset_code === "DEMO");
console.log(`XLM: ${xlm?.balance}   DEMO: ${demo?.balance}   sponsored entries: ${created.num_sponsored}`);
if (xlm?.balance !== "0.0000000") throw new Error("expected a zero-XLM account");

console.log("\nOK: the account exists, holds no XLM, and paid for itself.");
