import {
  Asset,
  Operation,
  Transaction,
  TransactionBuilder,
} from "@stellar/stellar-sdk";

import type {
  ReserveExpectations,
  ReserveOp,
  ReserveRequest,
  QuotePayload,
} from "./types.js";

/**
 * Raised when the transaction the service built is not the one that was quoted
 * and approved.
 */
export class ReserveVerificationError extends Error {
  constructor(message: string) {
    super(`reserve: refusing to sign — ${message}`);
    this.name = "ReserveVerificationError";
  }
}

function fail(message: string): never {
  throw new ReserveVerificationError(message);
}

/**
 * stellar-sdk 12–14 export `fromXDR`. 17 renamed it to `fromXdr`. Freighter
 * aliases `@stellar/stellar-sdk` onto that 17 build, so this has to accept both.
 */
function transactionFromXdr(xdrBase64: string, network: string): Transaction {
  const builder = TransactionBuilder as typeof TransactionBuilder & {
    fromXdr?: (envelope: string, networkPassphrase: string) => Transaction;
    fromXDR?: (envelope: string, networkPassphrase: string) => Transaction;
  };
  const parse = builder.fromXdr ?? builder.fromXDR;
  if (typeof parse !== "function") {
    fail("stellar-sdk cannot parse a transaction envelope");
  }
  return parse.call(TransactionBuilder, xdrBase64, network) as Transaction;
}

/** Decimal token amount as a whole number of stroops. */
export function amountToStroops(amount: string): string {
  const trimmed = amount.trim();
  if (!/^-?\d+(\.\d+)?$/.test(trimmed)) {
    fail(`${amount} is not an amount`);
  }
  const negative = trimmed.startsWith("-");
  const [whole = "0", frac = ""] = (negative ? trimmed.slice(1) : trimmed).split(".");
  const stroops = BigInt(whole) * 10_000_000n + BigInt(frac.padEnd(7, "0").slice(0, 7));
  if (negative) fail(`${amount} is negative`);
  return stroops.toString();
}

/** Stroops as the decimal string Stellar operations carry. */
export function stroopsToAmount(stroops: number | string): string {
  const value = BigInt(stroops);
  const sign = value < 0n ? "-" : "";
  const abs = value < 0n ? -value : value;
  return `${sign}${abs / 10_000_000n}.${(abs % 10_000_000n).toString().padStart(7, "0")}`;
}

function sameAmount(a: string, b: string): boolean {
  // "1" and "1.0000000" are the same number of stroops.
  const toStroops = (s: string) => {
    const [whole = "0", frac = ""] = s.split(".");
    return BigInt(whole) * 10_000_000n + BigInt(frac.padEnd(7, "0").slice(0, 7));
  };
  return toStroops(a) === toStroops(b);
}

function assetOf(canonical: string): Asset {
  if (canonical === "native") return Asset.native();
  const [code, issuer] = canonical.split(":");
  if (!code || !issuer) fail(`unreadable asset ${canonical}`);
  return new Asset(code, issuer);
}

function sameAsset(op: Asset, canonical: string): boolean {
  const want = assetOf(canonical);
  return op.equals(want);
}

function sameAssetString(a: string, b: string): boolean {
  return assetOf(a).equals(assetOf(b));
}

/**
 * A whole, non-negative number of stroops.
 *
 * Everything numeric in a payload arrives from the network, so a fraction or a
 * `NaN` has to become a refusal to sign rather than a `TypeError` out of
 * `BigInt` that no caller is catching.
 */
function stroopsOf(value: number | string, what: string): bigint {
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) fail(`${what} is not a whole number of stroops (${value})`);
  } else if (!/^\d+$/.test(value.trim())) {
    fail(`${what} is not a whole number of stroops (${value})`);
  }
  const stroops = BigInt(typeof value === "number" ? value : value.trim());
  if (stroops < 0n) fail(`${what} is negative (${value})`);
  return stroops;
}

function sameLimit(got: string | undefined, want: string | undefined): boolean {
  if (got === undefined || want === undefined) return got === want;
  return sameAmount(got, want);
}

/** Whether two operations, as the caller writes them, are the same operation. */
function sameOp(got: ReserveOp, want: ReserveOp): boolean {
  switch (want.type) {
    case "create_account":
      return got.type === "create_account" && got.destination === want.destination;
    case "payment":
      return (
        got.type === "payment" &&
        got.destination === want.destination &&
        sameAssetString(got.asset, want.asset) &&
        sameAmount(got.amount, want.amount)
      );
    case "change_trust":
      return (
        got.type === "change_trust" &&
        sameAssetString(got.asset, want.asset) &&
        sameLimit(got.limit, want.limit)
      );
    case "claim_balance":
      return (
        got.type === "claim_balance" &&
        got.balance_id.toLowerCase() === want.balance_id.toLowerCase()
      );
    case "path_payment_strict_send":
      return (
        got.type === "path_payment_strict_send" &&
        got.destination === want.destination &&
        sameAssetString(got.send_asset, want.send_asset) &&
        sameAmount(got.send_amount, want.send_amount) &&
        sameAssetString(got.dest_asset, want.dest_asset) &&
        sameAmount(got.dest_min, want.dest_min) &&
        (got.path ?? []).length === (want.path ?? []).length &&
        (got.path ?? []).every((hop, i) => sameAssetString(hop, (want.path ?? [])[i]!))
      );
  }
}

/**
 * Check that the quote answers the request that was actually made.
 *
 * Everything else in this module compares the built transaction against the
 * payload — that is, the service against itself, which a compromised service
 * satisfies trivially. This is the check that anchors the payload to something
 * the service did not choose.
 */
export function verifyQuote(
  payload: QuotePayload,
  request: ReserveRequest,
  expected: ReserveExpectations = {},
): void {
  if (expected.networkPassphrase !== undefined && payload.network !== expected.networkPassphrase) {
    fail(`the quote is for "${payload.network}", not "${expected.networkPassphrase}"`);
  }
  if (expected.sponsor !== undefined && payload.sponsor !== expected.sponsor) {
    fail(`the fee would go to ${payload.sponsor}, not the expected ${expected.sponsor}`);
  }
  if (payload.source !== request.source) {
    fail(`the quote is for ${payload.source}, not ${request.source}`);
  }
  if (!sameAssetString(payload.fee_token, request.feeToken)) {
    fail(`the fee would be taken in ${payload.fee_token}, not ${request.feeToken}`);
  }

  // The spend ceiling. `send_max_stroops` is what the path payment may take
  // from the user.
  const cap = stroopsOf(request.maxSendStroops, "maxSendStroops");
  const quoted = stroopsOf(payload.send_max_stroops, "the quoted sendMax");
  if (quoted > cap) {
    fail(`the quote would spend up to ${quoted} stroops, over the ${cap} allowed`);
  }

  if (payload.ops.length !== request.ops.length) {
    fail(`the quote is for ${payload.ops.length} operations, ${request.ops.length} were requested`);
  }
  payload.ops.forEach((op, i) => {
    if (!sameOp(op, request.ops[i]!)) {
      fail(`the quote's operation ${i} is not the one requested (${op.type})`);
    }
  });
}

/**
 * Check that a built transaction does exactly what was asked and charges no
 * more than the caller allowed.
 *
 * This is the check that makes a compromised service unable to move a user's
 * money: the wallet never signs bytes it has not re-derived itself. It only
 * holds if `request` carries the caller's own inputs. Passing it values read
 * back out of `payload` turns every comparison below into a tautology.
 */
export function verifyTransaction(
  xdrBase64: string,
  payload: QuotePayload,
  request: ReserveRequest,
  expected: ReserveExpectations = {},
): Transaction {
  // First: is this payload even an answer to the request? Every check after
  // this one leans on the payload, so it has to be anchored before it is used.
  verifyQuote(payload, request, expected);

  let tx: Transaction;
  try {
    tx = transactionFromXdr(xdrBase64, payload.network);
  } catch (e) {
    fail(`the built transaction could not be parsed (${(e as Error).message})`);
  }
  if ("innerTransaction" in tx) fail("expected a plain transaction, got a fee bump");

  const expectedSource =
    payload.mode === "bootstrap" ? payload.channel || payload.sponsor : payload.source;
  if (tx.source !== expectedSource) {
    fail(`transaction source is ${tx.source}, expected ${expectedSource}`);
  }
  if (tx.sequence !== String(payload.sequence)) {
    fail(`sequence is ${tx.sequence}, expected ${payload.sequence}`);
  }

  verifyClassic(tx, payload, request.ops);
  return tx;
}

function verifyClassic(tx: Transaction, payload: QuotePayload, requested: ReserveOp[]): void {
  const ops = [...tx.operations];
  const sponsored = payload.reserve_stroops > 0;

  if (sponsored) {
    const first = ops.shift();
    if (!first || first.type !== "beginSponsoringFutureReserves") {
      fail("sponsored reserves were quoted but the sandwich is missing");
    }
    if (first.sponsoredId !== payload.source) {
      fail(`sponsorship names ${first.sponsoredId}, not ${payload.source}`);
    }
    // CAP-33: the operation source is the sponsor. When the channel *is* the
    // sponsor the field is empty and the transaction source fills it in.
    const beginSource = first.source ?? tx.source;
    if (beginSource !== payload.sponsor) {
      fail(`the sandwich is opened by ${beginSource}, not the sponsor ${payload.sponsor}`);
    }
  }

  // Every transaction pays for itself, including the one that creates the
  // account: it pays out of the funds it claims.
  {
    const feeOp = ops.pop();
    if (!feeOp || feeOp.type !== "pathPaymentStrictReceive") {
      fail("the quoted fee payment is missing");
    }
    // In bootstrap the transaction source is a lane, so the payment must
    // name the user; otherwise the lane would be paying itself.
    const expectedPayer = payload.mode === "bootstrap" ? payload.source : undefined;
    if (feeOp.source !== expectedPayer) {
      fail(`the fee would be paid by ${feeOp.source ?? "the transaction source"}`);
    }
    if (feeOp.destination !== payload.sponsor) {
      fail(`the fee would go to ${feeOp.destination}, not ${payload.sponsor}`);
    }
    if (!feeOp.destAsset.isNative()) fail("the fee must be collected in XLM");
    if (!sameAmount(feeOp.destAmount, stroopsToAmount(payload.charge_stroops))) {
      fail(
        `the fee is ${feeOp.destAmount} XLM, but ${stroopsToAmount(payload.charge_stroops)} was quoted`,
      );
    }
    if (!sameAmount(feeOp.sendMax, stroopsToAmount(payload.send_max_stroops))) {
      fail(`sendMax is ${feeOp.sendMax}, but ${stroopsToAmount(payload.send_max_stroops)} was quoted`);
    }
    if (!sameAsset(feeOp.sendAsset, payload.fee_token)) {
      fail(`the fee would be taken in ${feeOp.sendAsset.getCode()}, not ${payload.fee_token}`);
    }
    // The route is what decides how much of `sendMax` actually gets spent, so
    // it has to be the quoted one. What bounds the loss is `sendMax` itself,
    // which `verifyQuote` has already held to the caller's ceiling.
    const route = feeOp.path ?? [];
    if (route.length !== payload.path.length) {
      fail(`the fee would be routed through ${route.length} hops, ${payload.path.length} were quoted`);
    }
    route.forEach((hop, i) => {
      if (!sameAsset(hop, payload.path[i]!)) {
        fail(`hop ${i} of the route is ${hop.getCode()}, not the quoted ${payload.path[i]}`);
      }
    });
  }

  if (sponsored) {
    const last = ops.pop();
    if (!last || last.type !== "endSponsoringFutureReserves") {
      fail("the sponsorship sandwich is not closed");
    }
    if (last.source !== payload.source) {
      fail("the sponsorship must be closed by the sponsored account");
    }
  }

  if (ops.length !== requested.length) {
    fail(`transaction has ${ops.length} operations, ${requested.length} were requested`);
  }

  ops.forEach((op, i) => {
    const want = requested[i]!;
    // Anything sourced by a third account would act on someone else's behalf.
    if (op.source !== undefined && op.source !== payload.source) {
      fail(`operation ${i} is sourced by ${op.source}`);
    }
    verifyOp(op, want, i);
  });
}

function verifyOp(op: Operation, want: ReserveOp, i: number): void {
  switch (want.type) {
    case "create_account": {
      if (op.type !== "createAccount") fail(`operation ${i} is ${op.type}, expected createAccount`);
      if (op.destination !== want.destination) {
        fail(`operation ${i} creates ${op.destination}, not ${want.destination}`);
      }
      // Reserves are sponsored, never funded out of anyone's balance.
      if (!sameAmount(op.startingBalance, "0")) {
        fail(`operation ${i} would fund ${op.startingBalance} XLM`);
      }
      return;
    }
    case "payment": {
      if (op.type !== "payment") fail(`operation ${i} is ${op.type}, expected payment`);
      if (op.destination !== want.destination) {
        fail(`operation ${i} pays ${op.destination}, not ${want.destination}`);
      }
      if (!sameAsset(op.asset, want.asset)) fail(`operation ${i} pays the wrong asset`);
      if (!sameAmount(op.amount, want.amount)) {
        fail(`operation ${i} pays ${op.amount}, not ${want.amount}`);
      }
      return;
    }
    case "claim_balance": {
      if (op.type !== "claimClaimableBalance") {
        fail(`operation ${i} is ${op.type}, expected claimClaimableBalance`);
      }
      if (op.balanceId !== want.balance_id) {
        fail(`operation ${i} claims ${op.balanceId}, not ${want.balance_id}`);
      }
      return;
    }
    case "change_trust": {
      if (op.type !== "changeTrust") fail(`operation ${i} is ${op.type}, expected changeTrust`);
      const line = op.line as Asset;
      if (!("getCode" in line) || !sameAsset(line, want.asset)) {
        fail(`operation ${i} trusts the wrong asset`);
      }
      if (want.limit !== undefined && !sameAmount(op.limit, want.limit)) {
        fail(`operation ${i} sets limit ${op.limit}, not ${want.limit}`);
      }
      return;
    }
    case "path_payment_strict_send": {
      if (op.type !== "pathPaymentStrictSend") {
        fail(`operation ${i} is ${op.type}, expected pathPaymentStrictSend`);
      }
      if (op.destination !== want.destination) {
        fail(`operation ${i} pays ${op.destination}, not ${want.destination}`);
      }
      if (!sameAsset(op.sendAsset, want.send_asset)) fail(`operation ${i} sends the wrong asset`);
      if (!sameAmount(op.sendAmount, want.send_amount)) {
        fail(`operation ${i} sends ${op.sendAmount}, not ${want.send_amount}`);
      }
      if (!sameAsset(op.destAsset, want.dest_asset)) fail(`operation ${i} buys the wrong asset`);
      if (!sameAmount(op.destMin, want.dest_min)) {
        fail(`operation ${i} requires ${op.destMin}, not ${want.dest_min}`);
      }
      const hops = op.path ?? [];
      const quoted = want.path ?? [];
      if (hops.length !== quoted.length) {
        fail(`operation ${i} routes through ${hops.length} hops, ${quoted.length} were requested`);
      }
      hops.forEach((hop, h) => {
        if (!sameAsset(hop, quoted[h]!)) {
          fail(`operation ${i} hop ${h} is ${hop.getCode()}, not ${quoted[h]}`);
        }
      });
      return;
    }
  }
}
