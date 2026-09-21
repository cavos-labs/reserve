import { describe, expect, it } from "vitest";
import {
  Account,
  Asset,
  BASE_FEE,
  Claimant,
  Keypair,
  Networks,
  Operation,
  TransactionBuilder,
} from "@stellar/stellar-sdk";

import { claimableClaimants } from "../src/claimable.js";

import { verifyTransaction, ReserveVerificationError } from "../src/verify.js";
import type { ReserveRequest, QuotePayload } from "../src/types.js";

const USER = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";
const SPONSOR = "GBAW5XGWORWVFE2XTJYDTLDHXTY2Q2MO73HYCGB3XMFMQ562Q2W2GJQX";
const ISSUER = "GAY5PRAHJ2HIYBYCLZXTHID6SPVELOOYH2LBPH3LD4RUMXUW3DOYTLXW";
const USDC = `USDC:${ISSUER}`;
const SEQUENCE = "19363546521403393";
const BALANCE_ID = "00000000da0d57da7d4850e7fc10d2a9d0ebc731f7afb40574c03395b17d49149b91f5be";

function payload(over: Partial<QuotePayload> = {}): QuotePayload {
  return {
    network: Networks.TESTNET,
    mode: "sponsored",
    source: USER,
    sponsor: SPONSOR,
    ops: [{ type: "payment", destination: SPONSOR, asset: USDC, amount: "1.0000000" }],
    fee_token: USDC,
    charge_stroops: 360,
    send_max_stroops: 303,
    path: [],
    reserve_stroops: 0,
    sequence: SEQUENCE,
    inner_fee_stroops: 200,
    min_time: 0,
    max_time: 1_800_000_000,
    expires_at_ledger: 120,
    ...over,
  };
}

function asset(canonical: string): Asset {
  if (canonical === "native") return Asset.native();
  const [code, issuer] = canonical.split(":");
  return new Asset(code!, issuer!);
}

/** Build what an honest service would return for a quote. */
function build(p: QuotePayload, tamper?: (b: TransactionBuilder) => void): string {
  const source = p.mode === "bootstrap" ? p.channel || p.sponsor : p.source;
  const builder = new TransactionBuilder(new Account(source, (BigInt(p.sequence) - 1n).toString()), {
    fee: String(p.inner_fee_stroops),
    networkPassphrase: p.network,
    timebounds: { minTime: p.min_time, maxTime: p.max_time },
  });

  if (p.reserve_stroops > 0) {
    builder.addOperation(
      Operation.beginSponsoringFutureReserves({
        sponsoredId: p.source,
        ...(source !== p.sponsor ? { source: p.sponsor } : {}),
      }),
    );
  }
  for (const op of p.ops) {
    if (op.type === "payment") {
      builder.addOperation(
        Operation.payment({
          destination: op.destination,
          asset: new Asset(op.asset.split(":")[0]!, op.asset.split(":")[1]!),
          amount: op.amount,
          ...(p.mode === "bootstrap" ? { source: p.source } : {}),
        }),
      );
    } else if (op.type === "create_account") {
      builder.addOperation(
        Operation.createAccount({ destination: op.destination, startingBalance: "0" }),
      );
    } else if (op.type === "claim_balance") {
      builder.addOperation(
        Operation.claimClaimableBalance({
          balanceId: op.balance_id,
          ...(p.mode === "bootstrap" ? { source: p.source } : {}),
        }),
      );
    } else if (op.type === "path_payment_strict_send") {
      builder.addOperation(
        Operation.pathPaymentStrictSend({
          sendAsset: asset(op.send_asset),
          sendAmount: op.send_amount,
          destination: op.destination,
          destAsset: asset(op.dest_asset),
          destMin: op.dest_min,
          path: (op.path ?? []).map(asset),
          ...(p.mode === "bootstrap" ? { source: p.source } : {}),
        }),
      );
    } else if (op.type === "create_claimable_balance") {
      builder.addOperation(
        Operation.createClaimableBalance({
          asset: asset(op.asset),
          amount: op.amount,
          claimants: claimableClaimants(p.source, op.destination),
          ...(p.mode === "bootstrap" ? { source: p.source } : {}),
        }),
      );
    } else {
      builder.addOperation(
        Operation.changeTrust({
          asset: new Asset(op.asset.split(":")[0]!, op.asset.split(":")[1]!),
          ...(p.mode === "bootstrap" ? { source: p.source } : {}),
        }),
      );
    }
  }
  if (p.reserve_stroops > 0) {
    builder.addOperation(Operation.endSponsoringFutureReserves({ source: p.source }));
  }
  {
    builder.addOperation(
      Operation.pathPaymentStrictReceive({
        sendAsset: asset(p.fee_token),
        sendMax: (p.send_max_stroops / 1e7).toFixed(7),
        destination: p.sponsor,
        destAsset: Asset.native(),
        destAmount: (p.charge_stroops / 1e7).toFixed(7),
        path: p.path.map(asset),
        ...(p.mode === "bootstrap" ? { source: p.source } : {}),
      }),
    );
  }
  tamper?.(builder);
  return builder.build().toEnvelope().toXDR("base64");
}

/**
 * The request an honest caller would have made to get `p`.
 *
 * Deriving it from the payload is only sound here because every test in this
 * file keeps the payload honest and tampers with the *built transaction*. The
 * case where the payload itself is the lie lives in `malicious-server.test.ts`,
 * and it needs a request the payload cannot influence.
 */
function asked(p: QuotePayload, over: Partial<ReserveRequest> = {}): ReserveRequest {
  return {
    source: p.source,
    feeToken: p.fee_token,
    maxSendStroops: p.send_max_stroops,
    ops: p.ops,
    ...over,
  };
}

function verify(xdr: string, p: QuotePayload, request?: ReserveRequest) {
  return verifyTransaction(xdr, p, request ?? asked(p));
}

describe("verifyTransaction", () => {
  it("accepts a claimable that the sender can reclaim", () => {
    const p = payload({
      ops: [
        {
          type: "create_claimable_balance",
          destination: SPONSOR,
          asset: USDC,
          amount: "1.0000000",
        },
      ],
      reserve_stroops: 10_000_000,
      inner_fee_stroops: 400,
    });
    expect(() => verify(build(p), p)).not.toThrow();
  });

  it("refuses a claimable without a sender reclaim", () => {
    const p = payload({
      ops: [
        {
          type: "create_claimable_balance",
          destination: SPONSOR,
          asset: USDC,
          amount: "1.0000000",
        },
      ],
      reserve_stroops: 10_000_000,
      inner_fee_stroops: 400,
    });
    const built = build(p, (b) => {
      (b as unknown as { operations: unknown[] }).operations = [];
      b.addOperation(
        Operation.beginSponsoringFutureReserves({
          sponsoredId: USER,
          source: SPONSOR,
        }),
      );
      b.addOperation(
        Operation.createClaimableBalance({
          asset: new Asset("USDC", ISSUER),
          amount: "1.0000000",
          claimants: [new Claimant(SPONSOR, Claimant.predicateUnconditional())],
        }),
      );
      b.addOperation(Operation.endSponsoringFutureReserves({ source: USER }));
      b.addOperation(
        Operation.pathPaymentStrictReceive({
          sendAsset: new Asset("USDC", ISSUER),
          sendMax: "0.0000303",
          destination: SPONSOR,
          destAsset: Asset.native(),
          destAmount: "0.0000360",
          path: [],
        }),
      );
    });
    expect(() => verify(built, p)).toThrow(/claimants/);
  });

  it("accepts a classic DEX swap", () => {
    const p = payload({
      ops: [
        {
          type: "path_payment_strict_send",
          destination: USER,
          send_asset: USDC,
          send_amount: "1.0000000",
          dest_asset: "native",
          dest_min: "5.0000000",
          path: [],
        },
      ],
    });
    expect(() => verify(build(p), p)).not.toThrow();
  });

  it("accepts the transaction that was quoted", () => {
    const p = payload();
    expect(() => verify(build(p), p)).not.toThrow();
  });

  it("parses envelopes when stellar-sdk only exposes fromXdr", () => {
    const p = payload();
    const xdr = build(p);
    const proto = TransactionBuilder as typeof TransactionBuilder & {
      fromXdr?: typeof TransactionBuilder.fromXDR;
      fromXDR?: typeof TransactionBuilder.fromXDR;
    };
    const originalXdr = proto.fromXdr;
    const originalXDR = proto.fromXDR;
    try {
      proto.fromXdr = originalXDR ?? originalXdr;
      delete proto.fromXDR;
      expect(() => verify(xdr, p)).not.toThrow();
    } finally {
      proto.fromXdr = originalXdr;
      proto.fromXDR = originalXDR;
    }
  });

  it("accepts a sponsored trustline", () => {
    const p = payload({
      ops: [{ type: "change_trust", asset: USDC }],
      reserve_stroops: 5_000_000,
      inner_fee_stroops: 400,
    });
    expect(() => verify(build(p), p)).not.toThrow();
  });

  it("accepts a bootstrap sourced by a channel account", () => {
    const channel = Keypair.random().publicKey();
    const p = payload({
      mode: "bootstrap",
      channel,
      ops: [
        { type: "create_account", destination: USER },
        { type: "claim_balance", balance_id: BALANCE_ID },
      ],
      reserve_stroops: 10_000_000,
      charge_stroops: 10_056_000,
      send_max_stroops: 1_805_000,
    });
    expect(() => verify(build(p), p)).not.toThrow();
  });

  it("refuses a channel bootstrap whose sandwich is not opened by the sponsor", () => {
    const channel = Keypair.random().publicKey();
    const p = payload({
      mode: "bootstrap",
      channel,
      ops: [
        { type: "create_account", destination: USER },
        { type: "claim_balance", balance_id: BALANCE_ID },
      ],
      reserve_stroops: 10_000_000,
      charge_stroops: 10_056_000,
      send_max_stroops: 1_805_000,
    });
    const built = build(p, (b) => {
      (b as unknown as { operations: unknown[] }).operations = [];
      b.addOperation(Operation.beginSponsoringFutureReserves({ sponsoredId: USER }));
      b.addOperation(Operation.createAccount({ destination: USER, startingBalance: "0" }));
      b.addOperation(Operation.claimClaimableBalance({ balanceId: BALANCE_ID, source: USER }));
      b.addOperation(Operation.endSponsoringFutureReserves({ source: USER }));
      b.addOperation(
        Operation.pathPaymentStrictReceive({
          sendAsset: new Asset("USDC", ISSUER),
          sendMax: "0.1805000",
          destination: SPONSOR,
          destAsset: Asset.native(),
          destAmount: "1.0056000",
          path: [],
          source: USER,
        }),
      );
    });
    expect(() => verify(built, p)).toThrow(/opened by/);
  });

  it("accepts a new account that pays for itself out of what it claims", () => {
    const p = payload({
      mode: "bootstrap",
      ops: [
        { type: "create_account", destination: USER },
        { type: "change_trust", asset: USDC },
        { type: "claim_balance", balance_id: BALANCE_ID },
      ],
      reserve_stroops: 15_000_000,
      charge_stroops: 15_056_000,
      send_max_stroops: 2_705_000,
    });
    expect(() => verify(build(p), p)).not.toThrow();
  });

  it("refuses a claim of somebody else's money", () => {
    const p = payload({
      mode: "bootstrap",
      ops: [
        { type: "create_account", destination: USER },
        { type: "change_trust", asset: USDC },
        { type: "claim_balance", balance_id: BALANCE_ID },
      ],
      reserve_stroops: 15_000_000,
      charge_stroops: 15_056_000,
      send_max_stroops: 2_705_000,
    });
    const other = `00000000${"ab".repeat(32)}`;
    const built = build({
      ...p,
      ops: [
        { type: "create_account", destination: USER },
        { type: "change_trust", asset: USDC },
        { type: "claim_balance", balance_id: other },
      ],
    });
    expect(() => verify(built, p)).toThrow(/claims/);
  });

  it("refuses a fee larger than the one quoted", () => {
    const p = payload();
    // The service quotes 360 stroops and builds a transaction taking more.
    const built = build({ ...p, charge_stroops: 5_000_000, send_max_stroops: 6_000_000 });
    expect(() => verify(built, p)).toThrow(ReserveVerificationError);
  });

  it("refuses a fee redirected elsewhere", () => {
    const p = payload();
    const thief = Keypair.random().publicKey();
    const built = build({ ...p, sponsor: thief });
    expect(() => verify(built, p)).toThrow(/the fee would go to/);
  });

  it("refuses an injected operation", () => {
    const p = payload();
    // The classic attack: slip in a signer the user never agreed to.
    const built = build(p, (b) =>
      b.addOperation(
        Operation.setOptions({
          signer: { ed25519PublicKey: Keypair.random().publicKey(), weight: 10 },
        }),
      ),
    );
    expect(() => verify(built, p)).toThrow(ReserveVerificationError);
  });

  it("refuses a rewritten payment", () => {
    const p = payload();
    const built = build({
      ...p,
      ops: [{ type: "payment", destination: SPONSOR, asset: USDC, amount: "500.0000000" }],
    });
    expect(() => verify(built, p)).toThrow(/pays 500/);
  });

  it("refuses a starting balance that drains the source", () => {
    const p = payload({
      mode: "bootstrap",
      ops: [
        { type: "create_account", destination: USER },
        { type: "claim_balance", balance_id: BALANCE_ID },
      ],
      reserve_stroops: 10_000_000,
      charge_stroops: 10_056_000,
      send_max_stroops: 1_805_000,
    });
    const built = build(p, (b) => {
      // Rebuild by hand with a funded createAccount.
      (b as unknown as { operations: unknown[] }).operations = [];
      b.addOperation(Operation.beginSponsoringFutureReserves({ sponsoredId: USER }));
      b.addOperation(Operation.createAccount({ destination: USER, startingBalance: "100" }));
      b.addOperation(Operation.claimClaimableBalance({ balanceId: BALANCE_ID, source: USER }));
      b.addOperation(Operation.endSponsoringFutureReserves({ source: USER }));
      b.addOperation(
        Operation.pathPaymentStrictReceive({
          sendAsset: new Asset("USDC", ISSUER),
          sendMax: "0.1805000",
          destination: SPONSOR,
          destAsset: Asset.native(),
          destAmount: "1.0056000",
          path: [],
          source: USER,
        }),
      );
    });
    expect(() => verify(built, p)).toThrow(/would fund/);
  });

  it("refuses a wrong sequence number or source", () => {
    const p = payload();
    expect(() => verify(build({ ...p, sequence: "999" }), p)).toThrow(/sequence/);
    const other = Keypair.random().publicKey();
    expect(() => verify(build({ ...p, source: other, sponsor: p.sponsor }), p)).toThrow(
      /transaction source/,
    );
  });

  it("refuses a sponsorship for somebody else", () => {
    const p = payload({ ops: [{ type: "change_trust", asset: USDC }], reserve_stroops: 5_000_000 });
    const built = build(p, (b) => {
      (b as unknown as { operations: unknown[] }).operations = [];
      b.addOperation(
        Operation.beginSponsoringFutureReserves({ sponsoredId: Keypair.random().publicKey() }),
      );
      b.addOperation(Operation.changeTrust({ asset: new Asset("USDC", ISSUER) }));
      b.addOperation(Operation.endSponsoringFutureReserves({ source: USER }));
      b.addOperation(
        Operation.pathPaymentStrictReceive({
          sendAsset: new Asset("USDC", ISSUER),
          sendMax: "0.0000303",
          destination: SPONSOR,
          destAsset: Asset.native(),
          destAmount: "0.0000360",
          path: [],
        }),
      );
    });
    expect(() => verify(built, p)).toThrow(/sponsorship names/);
  });

  it("refuses a fee taken in a different token", () => {
    const p = payload();
    const built = build({ ...p, fee_token: `EURC:${ISSUER}` });
    expect(() => verify(built, p)).toThrow(/taken in/);
  });

  it("refuses a route the quote did not name", () => {
    // The route decides how much of sendMax is actually spent, so a service
    // that quotes a direct conversion and builds a detour is caught.
    const p = payload();
    const built = build({ ...p, path: [`EURC:${ISSUER}`] });
    expect(() => verify(built, p)).toThrow(/routed through/);
  });

  it("refuses a route whose hops are not the quoted ones", () => {
    const p = payload({ path: [`EURC:${ISSUER}`] });
    const built = build({ ...p, path: [`SCAM:${ISSUER}`] });
    expect(() => verify(built, p)).toThrow(/hop 0 of the route/);
  });

  it("accepts the quoted route", () => {
    const p = payload({ path: [`EURC:${ISSUER}`, "native"] });
    expect(() => verify(build(p), p)).not.toThrow();
  });

  it("checks the base fee constant is not what protects us", () => {
    // A transaction bidding a huge inclusion fee is still fine for the user:
    // the sponsor pays it. Nothing here should reject it.
    const p = payload({ inner_fee_stroops: Number(BASE_FEE) * 500 });
    expect(() => verify(build(p), p)).not.toThrow();
  });
});
