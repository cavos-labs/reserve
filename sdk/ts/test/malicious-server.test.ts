/**
 * The attack `verify.test.ts` cannot see.
 *
 * Those tests keep the quote payload honest and tamper with the built
 * transaction, which exercises payload-against-XDR agreement. A compromised
 * service has no reason to disagree with itself: it returns a payload that
 * describes something the user never asked for, and a transaction that matches
 * that payload perfectly. Every check that compares the two is satisfied.
 *
 * So the request here is written out by hand and never derived from anything
 * the service returned. That is the whole point — a helper that reads the
 * request back out of the payload turns all of this green while proving
 * nothing, which is exactly how the hole got in.
 */
import { describe, expect, it } from "vitest";
import { Account, Asset, Keypair, Networks, Operation, TransactionBuilder } from "@stellar/stellar-sdk";

import { verifyQuote, verifyTransaction, ReserveVerificationError } from "../src/verify.js";
import type { ReserveRequest, QuotePayload } from "../src/types.js";

const USER = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";
const SPONSOR = "GBAW5XGWORWVFE2XTJYDTLDHXTY2Q2MO73HYCGB3XMFMQ562Q2W2GJQX";
const ISSUER = "GAY5PRAHJ2HIYBYCLZXTHID6SPVELOOYH2LBPH3LD4RUMXUW3DOYTLXW";
const USDC = `USDC:${ISSUER}`;
const THIEF = Keypair.fromRawEd25519Seed(Buffer.alloc(32, 7)).publicKey();

function asset(canonical: string): Asset {
  if (canonical === "native") return Asset.native();
  const [code, issuer] = canonical.split(":");
  return new Asset(code!, issuer!);
}

/**
 * What the user actually asked for: send 1 USDC to the sponsor, and spend no
 * more than 0.05 USDC on fees. Fixed, and independent of every payload below.
 */
const REQUEST: ReserveRequest = {
  source: USER,
  feeToken: USDC,
  maxSendStroops: 500_000,
  ops: [{ type: "payment", destination: SPONSOR, asset: USDC, amount: "1.0000000" }],
};

/** The quote an honest service would seal for `REQUEST`. */
function honest(over: Partial<QuotePayload> = {}): QuotePayload {
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
    sequence: "19363546521403393",
    inner_fee_stroops: 200,
    min_time: 0,
    max_time: 1_800_000_000,
    expires_at_ledger: 120,
    ...over,
  };
}

/**
 * Build the transaction a service would build *for its own payload*. Not a
 * tampered transaction: an internally consistent one, which is what makes the
 * attack invisible to a payload-against-XDR check.
 */
function buildFor(p: QuotePayload): string {
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
          asset: asset(op.asset),
          amount: op.amount,
        }),
      );
    } else if (op.type === "change_trust") {
      builder.addOperation(Operation.changeTrust({ asset: asset(op.asset) }));
    } else if (op.type === "create_account") {
      builder.addOperation(
        Operation.createAccount({ destination: op.destination, startingBalance: "0" }),
      );
    } else {
      builder.addOperation(Operation.claimClaimableBalance({ balanceId: op.balance_id }));
    }
  }
  if (p.reserve_stroops > 0) {
    builder.addOperation(Operation.endSponsoringFutureReserves({ source: p.source }));
  }
  builder.addOperation(
    Operation.pathPaymentStrictReceive({
      sendAsset: asset(p.fee_token),
      sendMax: (p.send_max_stroops / 1e7).toFixed(7),
      destination: p.sponsor,
      destAsset: Asset.native(),
      destAmount: (p.charge_stroops / 1e7).toFixed(7),
      path: p.path.map(asset),
    }),
  );
  return builder.build().toEnvelope().toXDR("base64");
}

/** The service answers with `p`, and builds a transaction that matches it. */
function answersWith(p: QuotePayload, expectations = {}) {
  return () => verifyTransaction(buildFor(p), p, REQUEST, expectations);
}

describe("a service that lies in the quote itself", () => {
  it("is accepted when it answers the request honestly", () => {
    expect(answersWith(honest())).not.toThrow();
  });

  it("cannot swap the operation for one paying somebody else", () => {
    const evil = honest({
      ops: [{ type: "payment", destination: THIEF, asset: USDC, amount: "500.0000000" }],
    });
    // The transaction and the payload agree completely. Only the request disagrees.
    expect(answersWith(evil)).toThrow(/operation 0 is not the one requested/);
  });

  it("cannot inflate the fee past the caller's ceiling", () => {
    const evil = honest({ charge_stroops: 900_000_000, send_max_stroops: 950_000_000 });
    expect(answersWith(evil)).toThrow(/over the 500000 allowed/);
  });

  it("cannot redirect the fee to another account", () => {
    expect(answersWith(honest({ sponsor: THIEF }), { sponsor: SPONSOR })).toThrow(
      /not the expected/,
    );
  });

  it("cannot take the fee in a token the caller did not choose", () => {
    const evil = honest({ fee_token: `SCAM:${ISSUER}` });
    expect(answersWith(evil)).toThrow(/taken in SCAM/);
  });

  it("cannot inject an extra operation", () => {
    const evil = honest({
      ops: [
        { type: "payment", destination: SPONSOR, asset: USDC, amount: "1.0000000" },
        { type: "change_trust", asset: `SCAM:${ISSUER}` },
      ],
      reserve_stroops: 5_000_000,
    });
    expect(answersWith(evil)).toThrow(/2 operations, 1 were requested/);
  });

  it("cannot drop the operation the caller asked for", () => {
    expect(answersWith(honest({ ops: [] }))).toThrow(/0 operations, 1 were requested/);
  });

  it("cannot quote for a different account", () => {
    const evil = honest({ source: THIEF, ops: REQUEST.ops });
    expect(answersWith(evil)).toThrow(/the quote is for/);
  });

  it("cannot move the user to another network", () => {
    const evil = honest({ network: Networks.PUBLIC });
    expect(answersWith(evil, { networkPassphrase: Networks.TESTNET })).toThrow(/the quote is for "/);
  });

  it("cannot silently raise the limit on a trustline the caller bounded", () => {
    const bounded: ReserveRequest = {
      ...REQUEST,
      ops: [{ type: "change_trust", asset: USDC, limit: "100.0000000" }],
    };
    const evil = honest({
      ops: [{ type: "change_trust", asset: USDC, limit: "922337203685.4775807" }],
      reserve_stroops: 5_000_000,
    });
    expect(() => verifyTransaction(buildFor(evil), evil, bounded)).toThrow(
      /operation 0 is not the one requested/,
    );
  });
});

describe("the spend ceiling", () => {
  it("is what bounds a hostile route, since the route itself cannot be re-priced offline", () => {
    // A detour through an illiquid asset pushes the real spend toward sendMax.
    // The client cannot tell a bad route from a good one without doing its own
    // path finding, so the defence is the ceiling, not the route.
    const routed = honest({ path: [`SCAM:${ISSUER}`], send_max_stroops: 499_999 });
    expect(answersWith(routed)).not.toThrow();

    const overCeiling = honest({ path: [`SCAM:${ISSUER}`], send_max_stroops: 500_001 });
    expect(answersWith(overCeiling)).toThrow(ReserveVerificationError);
  });

  // Checked against `verifyQuote` rather than a built transaction: these
  // payloads are ones no builder will accept, so there is no XDR to pair them
  // with. What matters is that they come back as a refusal to sign and not as
  // a `RangeError` out of `BigInt` that no caller is catching.
  it("refuses a quoted amount that is not a whole number of stroops", () => {
    for (const send_max_stroops of [1.5, Number.NaN, Number.POSITIVE_INFINITY, 1e300]) {
      expect(() => verifyQuote(honest({ send_max_stroops }), REQUEST)).toThrow(
        ReserveVerificationError,
      );
      expect(() => verifyQuote(honest({ send_max_stroops }), REQUEST)).toThrow(
        /whole number of stroops/,
      );
    }
  });

  it("refuses a negative quoted amount", () => {
    expect(() => verifyQuote(honest({ send_max_stroops: -1 }), REQUEST)).toThrow(
      /quoted sendMax is negative/,
    );
  });

  it("refuses a ceiling the caller stated badly, rather than ignoring it", () => {
    expect(() => verifyQuote(honest(), { ...REQUEST, maxSendStroops: "lots" })).toThrow(
      /maxSendStroops is not a whole number of stroops/,
    );
  });

  it("accepts a ceiling given as a string, for amounts past Number.MAX_SAFE_INTEGER", () => {
    expect(() =>
      verifyQuote(honest(), { ...REQUEST, maxSendStroops: "9007199254740993" }),
    ).not.toThrow();
  });
});
