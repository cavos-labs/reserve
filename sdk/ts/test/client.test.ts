/**
 * The hole lived in `Reserve.build`, not in `verifyTransaction`.
 *
 * `malicious-server.test.ts` calls the verifier with a request written by
 * hand. That is the right check for the verifier. This file is the check that
 * the client actually hands it that request, and not values read back out of
 * the quote the service just sealed — which is how the tautology got in.
 */
import { describe, expect, it } from "vitest";
import { Account, Asset, Keypair, Networks, Operation, TransactionBuilder } from "@stellar/stellar-sdk";

import { Reserve, ReserveError } from "../src/client.js";
import { ReserveVerificationError } from "../src/verify.js";
import type { Quote, QuotePayload } from "../src/types.js";

const USER = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";
const SPONSOR = "GBAW5XGWORWVFE2XTJYDTLDHXTY2Q2MO73HYCGB3XMFMQ562Q2W2GJQX";
const ISSUER = "GAY5PRAHJ2HIYBYCLZXTHID6SPVELOOYH2LBPH3LD4RUMXUW3DOYTLXW";
const USDC = `USDC:${ISSUER}`;
const THIEF = Keypair.fromRawEd25519Seed(Buffer.alloc(32, 7)).publicKey();

const ASKED = {
  source: USER,
  feeToken: USDC,
  maxSendStroops: 500_000,
  ops: [{ type: "payment" as const, destination: SPONSOR, asset: USDC, amount: "1.0000000" }],
};

function honest(over: Partial<QuotePayload> = {}): QuotePayload {
  return {
    network: Networks.TESTNET,
    mode: "sponsored",
    source: USER,
    sponsor: SPONSOR,
    ops: ASKED.ops,
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

function seal(payload: QuotePayload): string {
  const json = JSON.stringify(payload);
  const b64 = btoa(json).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  return `${b64}.fakesig`;
}

function asset(canonical: string): Asset {
  if (canonical === "native") return Asset.native();
  const [code, issuer] = canonical.split(":");
  return new Asset(code!, issuer!);
}

function buildFor(p: QuotePayload): string {
  const source = p.mode === "bootstrap" ? p.channel || p.sponsor : p.source;
  const builder = new TransactionBuilder(new Account(source, (BigInt(p.sequence) - 1n).toString()), {
    fee: String(p.inner_fee_stroops),
    networkPassphrase: p.network,
    timebounds: { minTime: p.min_time, maxTime: p.max_time },
  });
  for (const op of p.ops) {
    if (op.type === "payment") {
      builder.addOperation(
        Operation.payment({ destination: op.destination, asset: asset(op.asset), amount: op.amount }),
      );
    }
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

function quoteBody(payload: QuotePayload) {
  return {
    quote: seal(payload),
    mode: payload.mode,
    charge_stroops: payload.charge_stroops,
    send_max_stroops: payload.send_max_stroops,
    reserve_stroops: payload.reserve_stroops,
    slippage_bps: 50,
    creates_account: false,
    expires_at_ledger: payload.expires_at_ledger,
  };
}

function respond(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function client(
  handler: (path: string, body: unknown) => Response,
  options: { networkPassphrase?: string; sponsor?: string } = {},
): Reserve {
  return new Reserve({
    url: "http://reserve.test",
    networkPassphrase: options.networkPassphrase,
    sponsor: options.sponsor,
    fetch: async (input, init) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      const path = new URL(url).pathname;
      const body = init?.body ? JSON.parse(String(init.body)) : undefined;
      return handler(path, body);
    },
  });
}

describe("Reserve.quote", () => {
  it("keeps the caller's request on the quote, not the service's account of it", async () => {
    const reserve = client((path) => {
      expect(path).toBe("/v1/quote");
      return respond(200, quoteBody(honest()));
    });
    const quote = await reserve.quote(ASKED);
    expect(quote.request).toEqual(ASKED);
    expect(quote.request).not.toBe(quote.payload.ops);
  });

  it("refuses a quote whose operations are not the ones that were asked", async () => {
    const reserve = client(() =>
      respond(
        200,
        quoteBody(
          honest({
            ops: [{ type: "payment", destination: THIEF, asset: USDC, amount: "500.0000000" }],
          }),
        ),
      ),
    );
    await expect(reserve.quote(ASKED)).rejects.toThrow(ReserveVerificationError);
    await expect(reserve.quote(ASKED)).rejects.toThrow(/operation 0 is not the one requested/);
  });

  it("refuses a quote that would spend more than the caller allowed", async () => {
    const reserve = client(() =>
      respond(200, quoteBody(honest({ send_max_stroops: 500_001 }))),
    );
    await expect(reserve.quote(ASKED)).rejects.toThrow(/over the 500000 allowed/);
  });

  it("refuses a quote for another network when one was pinned", async () => {
    const reserve = client(() => respond(200, quoteBody(honest({ network: Networks.PUBLIC }))), {
      networkPassphrase: Networks.TESTNET,
    });
    await expect(reserve.quote(ASKED)).rejects.toThrow(/the quote is for "/);
  });

  it("refuses a quote that pays a different sponsor when one was pinned", async () => {
    const reserve = client(() => respond(200, quoteBody(honest({ sponsor: THIEF }))), {
      sponsor: SPONSOR,
    });
    await expect(reserve.quote(ASKED)).rejects.toThrow(/not the expected/);
  });

  it("surfaces an HTTP error from the service as ReserveError", async () => {
    const reserve = client(() =>
      respond(429, { error: "rate_limited", message: "too many requests" }),
    );
    await expect(reserve.quote(ASKED)).rejects.toBeInstanceOf(ReserveError);
  });
});

describe("Reserve.build", () => {
  function held(payload: QuotePayload): Quote {
    return {
      token: seal(payload),
      payload,
      request: ASKED,
      mode: payload.mode,
      chargeStroops: payload.charge_stroops,
      sendMaxStroops: payload.send_max_stroops,
      reserveStroops: payload.reserve_stroops,
      slippageBps: 50,
      createsAccount: false,
      expiresAtLedger: payload.expires_at_ledger,
    };
  }

  it("verifies against the held request, so a consistent lie is still a lie", async () => {
    const evil = honest({
      ops: [{ type: "payment", destination: THIEF, asset: USDC, amount: "500.0000000" }],
    });
    // The service builds a transaction that matches its own payload perfectly.
    const reserve = client((path) => {
      expect(path).toBe("/v1/build");
      return respond(200, {
        xdr: buildFor(evil),
        network_passphrase: evil.network,
        signers: [USER],
      });
    });
    await expect(reserve.build(held(evil))).rejects.toThrow(/operation 0 is not the one requested/);
  });

  it("refuses a passphrase the quote did not name", async () => {
    const payload = honest();
    const reserve = client(() =>
      respond(200, {
        xdr: buildFor(payload),
        network_passphrase: Networks.PUBLIC,
        signers: [USER],
      }),
    );
    await expect(reserve.build(held(payload))).rejects.toThrow(/asked to sign for/);
  });

  it("accepts an honest build", async () => {
    const payload = honest();
    const xdr = buildFor(payload);
    const reserve = client(() =>
      respond(200, { xdr, network_passphrase: payload.network, signers: [USER] }),
    );
    await expect(reserve.build(held(payload))).resolves.toEqual({
      xdr,
      networkPassphrase: Networks.TESTNET,
    });
  });
});

describe("Reserve shortcuts", () => {
  it("new Reserve(\"testnet\") pins the hosted URL and passphrase", async () => {
    let seen = "";
    const reserve = new Reserve({
      network: "testnet",
      fetch: async (input) => {
        seen = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        return respond(200, quoteBody(honest()));
      },
    });
    await reserve.quote(ASKED);
    expect(seen).toBe("https://reserve.cavos.xyz/testnet/v1/quote");
  });

  it("new Reserve(\"mainnet\") pins the hosted URL and passphrase", async () => {
    let seen = "";
    const reserve = new Reserve({
      network: "mainnet",
      fetch: async (input) => {
        seen = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        return respond(200, quoteBody(honest({ network: Networks.PUBLIC })));
      },
    });
    await reserve.quote(ASKED);
    expect(seen).toBe("https://reserve.cavos.xyz/mainnet/v1/quote");
  });

  it("connect pins the sponsor from /health", async () => {
    const reserve = await Reserve.connect({
      url: "http://reserve.test",
      network: "testnet",
      fetch: async (input) => {
        const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        if (url.endsWith("/health")) {
          return respond(200, { sponsor: SPONSOR, network: Networks.TESTNET });
        }
        return respond(200, quoteBody(honest({ sponsor: THIEF })));
      },
    });
    await expect(reserve.quote(ASKED)).rejects.toThrow(/not the expected/);
  });

  it("pay quotes a single payment", async () => {
    let asked: unknown;
    const reserve = client((path, body) => {
      if (path === "/v1/quote") {
        asked = body;
        return respond(200, quoteBody(honest()));
      }
      return respond(200, {
        xdr: buildFor(honest()),
        network_passphrase: Networks.TESTNET,
        signers: [USER],
        hash: "ab",
      });
    });
    await reserve.pay(
      {
        source: USER,
        destination: SPONSOR,
        amount: "1.0000000",
        token: USDC,
        maxSend: "0.05",
      },
      () => "signed",
    );
    expect(asked).toMatchObject({
      source: USER,
      fee_token: USDC,
      ops: [{ type: "payment", destination: SPONSOR, asset: USDC, amount: "1.0000000" }],
    });
  });

  it("activate quotes create + trust + claim", async () => {
    let asked: unknown;
    const reserve = client((path, body) => {
      if (path === "/v1/quote") {
        asked = body;
        return respond(
          200,
          quoteBody(
            honest({
              ops: [
                { type: "create_account", destination: USER },
                { type: "change_trust", asset: USDC },
                { type: "claim_balance", balance_id: "aa".repeat(36) },
              ],
            }),
          ),
        );
      }
      return respond(200, { hash: "ab" });
    });
    // activate calls send → quote then build. The quoted ops won't match
    // buildFor(honest()) so stop after quote by having build fail after we
    // captured the request. Easier: just quote via send and catch build.
    await reserve
      .activate(
        { address: USER, token: USDC, balanceId: "aa".repeat(36), maxSendStroops: 500_000 },
        () => "signed",
      )
      .catch(() => undefined);
    expect(asked).toMatchObject({
      source: USER,
      fee_token: USDC,
      ops: [
        { type: "create_account", destination: USER },
        { type: "change_trust", asset: USDC },
        { type: "claim_balance", balance_id: "aa".repeat(36) },
      ],
    });
  });
});
