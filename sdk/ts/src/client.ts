import { amountToStroops, verifyQuote, verifyTransaction, ReserveVerificationError } from "./verify.js";
import { HOSTED, horizonUrlOf, networkFromUrl, passphraseOf, TESTNET, PUBLIC, type NetworkName } from "./networks.js";
import type {
  ReserveExpectations,
  KnownToken,
  ReserveOp,
  ReserveRequest,
  Quote,
  QuotePayload,
  Signer,
  SubmitResult,
} from "./types.js";

export type { NetworkName } from "./networks.js";

export interface ReserveOptions {
  /** `testnet` or `mainnet`. Sets the hosted URL and pins the passphrase. */
  network?: NetworkName;
  /** Base URL of the service. Defaults to the hosted URL for `network`. */
  url?: string;
  /** Passed straight through to `fetch`; override for auth headers or proxies. */
  fetch?: typeof globalThis.fetch;
  headers?: Record<string, string>;
  /**
   * The network this client is for. When set, a quote for any other network is
   * refused. Worth setting: the passphrase is not carried inside a transaction
   * envelope, so the service picking a different one is otherwise silent.
   */
  networkPassphrase?: string;
  /**
   * The account expected to collect fees. When set, a quote paying anyone else
   * is refused. `Reserve.connect` pins this from `/health`.
   */
  sponsor?: string;
  /**
   * Horizon used by `pay` to see whether the destination can receive a Payment.
   * Defaults from `network` / the passphrase.
   */
  horizonUrl?: string;
}

export type QuoteArgs = {
  source: string;
  ops: ReserveOp[];
  feeToken?: string;
  /** Ceiling in the fee token's stroops. */
  maxSendStroops?: number | string;
  /** Ceiling as a decimal token amount (`"0.05"`). Ignored if `maxSendStroops` is set. */
  maxSend?: string;
};

export class ReserveError extends Error {
  constructor(
    message: string,
    readonly code: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "ReserveError";
  }
}

/** Decode the quote payload the service sealed. */
export function readQuote(token: string): QuotePayload {
  const [payload] = token.split(".");
  if (!payload) throw new ReserveVerificationError("the quote is malformed");
  const json = atob(payload.replace(/-/g, "+").replace(/_/g, "/"));
  return JSON.parse(json) as QuotePayload;
}

function resolve(input: NetworkName | ReserveOptions): Required<Pick<ReserveOptions, "url">> &
  ReserveOptions {
  if (input === "testnet" || input === "mainnet") {
    return { ...HOSTED[input], network: input };
  }
  const network = input.network ?? networkFromUrl(input.url ?? "") ?? undefined;
  const url = input.url?.replace(/\/$/, "") ?? (network ? HOSTED[network].url : undefined);
  if (!url) {
    throw new ReserveError(
      "pass a network (\"testnet\" or \"mainnet\") or a url",
      "invalid_request",
      0,
    );
  }
  const networkPassphrase =
    input.networkPassphrase ?? (network ? passphraseOf(network) : undefined);
  const horizonUrl =
    input.horizonUrl?.replace(/\/$/, "") ??
    (network
      ? horizonUrlOf(network)
      : networkPassphrase === TESTNET
        ? horizonUrlOf("testnet")
        : networkPassphrase === PUBLIC
          ? horizonUrlOf("mainnet")
          : undefined);
  return { ...input, url, network, networkPassphrase, horizonUrl };
}

function ceiling(request: QuoteArgs): number | string {
  if (request.maxSendStroops !== undefined) return request.maxSendStroops;
  if (request.maxSend !== undefined) return amountToStroops(request.maxSend);
  throw new ReserveError(
    "maxSend (token amount) or maxSendStroops is required",
    "invalid_request",
    0,
  );
}

function isQuote(value: Quote | QuoteArgs): value is Quote {
  return typeof (value as Quote).token === "string" && (value as Quote).request !== undefined;
}

export class Reserve {
  private readonly url: string;
  private readonly doFetch: typeof globalThis.fetch;
  private readonly headers: Record<string, string>;
  private readonly expected: ReserveExpectations;
  private readonly horizonUrl?: string;

  constructor(options: NetworkName | ReserveOptions) {
    const resolved = resolve(options);
    this.url = resolved.url;
    this.doFetch = resolved.fetch ?? globalThis.fetch.bind(globalThis);
    this.headers = resolved.headers ?? {};
    this.horizonUrl = resolved.horizonUrl;
    this.expected = {
      ...(resolved.networkPassphrase !== undefined
        ? { networkPassphrase: resolved.networkPassphrase }
        : {}),
      ...(resolved.sponsor !== undefined ? { sponsor: resolved.sponsor } : {}),
    };
  }

  /**
   * Hosted client with the passphrase pinned and the sponsor taken from
   * `/health`. Stronger than `new Reserve("testnet")` alone: a quote that
   * pays anyone else is refused.
   */
  static async connect(
    network: NetworkName | ReserveOptions = "testnet",
  ): Promise<Reserve> {
    const resolved = resolve(network);
    const doFetch = resolved.fetch ?? globalThis.fetch.bind(globalThis);
    const res = await doFetch(`${resolved.url}/health`, {
      headers: resolved.headers,
    });
    const health = (await res.json()) as { sponsor?: string; network?: string };
    return new Reserve({
      ...resolved,
      sponsor: resolved.sponsor ?? health.sponsor,
      networkPassphrase: resolved.networkPassphrase ?? health.network,
    });
  }

  private async post<T>(path: string, body: unknown): Promise<T> {
    const res = await this.doFetch(`${this.url}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json", ...this.headers },
      body: JSON.stringify(body),
    });
    const text = await res.text();
    if (!res.ok) {
      let code = "http_error";
      let message = text;
      try {
        const parsed = JSON.parse(text) as { error?: string; message?: string };
        code = parsed.error ?? code;
        message = parsed.message ?? message;
      } catch {
        // Not JSON; the raw body is the best error we have.
      }
      throw new ReserveError(message, code, res.status);
    }
    return JSON.parse(text) as T;
  }

  /**
   * Price a set of classic operations.
   *
   * `maxSend` / `maxSendStroops` is the most the caller is willing to let leave
   * the user's balance, in the fee token. It is required because nothing else
   * bounds the price: the fee payment is not one of `ops`, so a quote is
   * rejected here or not at all.
   */
  async quote(request: QuoteArgs): Promise<Quote> {
    const feeToken = request.feeToken ?? "native";
    const maxSendStroops = ceiling(request);
    const asked: ReserveRequest = {
      source: request.source,
      feeToken,
      maxSendStroops,
      ops: request.ops,
    };
    const res = await this.post<{
      quote: string;
      mode: Quote["mode"];
      charge_stroops: number;
      send_max_stroops: number;
      reserve_stroops: number;
      slippage_bps: number;
      creates_account: boolean;
      expires_at_ledger: number;
    }>("/v1/quote", {
      source: request.source,
      fee_token: feeToken,
      ops: request.ops,
    });
    const payload = readQuote(res.quote);
    // Fail here rather than at signing time: the caller can show a price only
    // once it is known to be a price for what they asked.
    verifyQuote(payload, asked, this.expected);
    return {
      token: res.quote,
      payload,
      request: asked,
      mode: res.mode,
      chargeStroops: res.charge_stroops,
      sendMaxStroops: res.send_max_stroops,
      reserveStroops: res.reserve_stroops,
      slippageBps: res.slippage_bps,
      createsAccount: res.creates_account,
      expiresAtLedger: res.expires_at_ledger,
    };
  }

  /**
   * Fetch the transaction for a quote **and verify it locally**.
   *
   * Never skip this by calling the endpoint directly: it is the only thing
   * standing between a wallet and blindly signing bytes a server chose.
   */
  async build(quote: Quote): Promise<{ xdr: string; networkPassphrase: string }> {
    const res = await this.post<{
      xdr: string;
      network_passphrase: string;
      signers: string[];
    }>("/v1/build", { quote: quote.token });
    // Against `quote.request` — the caller's own words. Reading these out of
    // `quote.payload` instead would compare the service against itself.
    verifyTransaction(res.xdr, quote.payload, quote.request, this.expected);
    // The wallet signs with this passphrase, and it is not covered by the XDR.
    if (res.network_passphrase !== quote.payload.network) {
      throw new ReserveVerificationError(
        `asked to sign for "${res.network_passphrase}" but the quote is for "${quote.payload.network}"`,
      );
    }
    return { xdr: res.xdr, networkPassphrase: res.network_passphrase };
  }

  async submit(quote: Quote, signedXdr: string): Promise<SubmitResult> {
    const res = await this.post<{ hash: string; ledger?: number }>("/v1/submit", {
      quote: quote.token,
      signed_xdr: signedXdr,
    });
    return { hash: res.hash, ledger: res.ledger };
  }

  /** build → verify → sign → submit. Pass a quote, or the request to quote first. */
  async send(quoteOrRequest: Quote | QuoteArgs, sign: Signer): Promise<SubmitResult> {
    const quote = isQuote(quoteOrRequest) ? quoteOrRequest : await this.quote(quoteOrRequest);
    const { xdr, networkPassphrase } = await this.build(quote);
    const signed = await sign(xdr, { networkPassphrase });
    return this.submit(quote, signed);
  }

  /** One payment, fee taken in the same token. Leaves a claimable if the dest cannot receive yet. */
  async pay(
    input: {
      source: string;
      destination: string;
      amount: string;
      token: string;
      maxSendStroops?: number | string;
      maxSend?: string;
    },
    sign: Signer,
  ): Promise<SubmitResult> {
    if (input.source === input.destination) {
      throw new ReserveError(
        "cannot leave a claimable balance for yourself",
        "invalid_request",
        0,
      );
    }
    const ready = await this.destinationReady(input.destination, input.token);
    return this.send(
      {
        source: input.source,
        feeToken: input.token,
        maxSendStroops: input.maxSendStroops,
        maxSend: input.maxSend,
        ops: ready
          ? [
              {
                type: "payment",
                destination: input.destination,
                asset: input.token,
                amount: input.amount,
              },
            ]
          : [
              {
                type: "create_claimable_balance",
                destination: input.destination,
                asset: input.token,
                amount: input.amount,
              },
            ],
      },
      sign,
    );
  }

  /**
   * Whether `destination` can take a Payment of `asset` right now: the account
   * exists, and for a credit asset it has a live trustline.
   */
  async destinationReady(destination: string, asset: string): Promise<boolean> {
    if (!this.horizonUrl) {
      throw new ReserveError(
        "pass a network so pay() can see whether the destination is ready",
        "invalid_request",
        0,
      );
    }
    const res = await this.doFetch(`${this.horizonUrl}/accounts/${destination}`, {
      headers: this.headers,
    });
    if (res.status === 404) return false;
    if (!res.ok) {
      throw new ReserveError(
        `horizon could not load ${destination} (${res.status})`,
        "horizon_error",
        res.status,
      );
    }
    if (asset === "native") return true;
    const [code, issuer] = asset.split(":");
    const account = (await res.json()) as {
      balances: {
        asset_type: string;
        asset_code?: string;
        asset_issuer?: string;
        is_authorized?: boolean;
        limit?: string;
      }[];
    };
    return account.balances.some((line) => {
      if (line.asset_code !== code || line.asset_issuer !== issuer) return false;
      if (line.is_authorized === false) return false;
      if (line.limit === "0" || line.limit === "0.0000000") return false;
      return true;
    });
  }

  /**
   * Create the account, open the trustline, and claim in one transaction.
   * Someone else must have left a claimable balance first.
   */
  async activate(
    input: {
      address: string;
      token: string;
      balanceId: string;
      maxSendStroops?: number | string;
      maxSend?: string;
    },
    sign: Signer,
  ): Promise<SubmitResult> {
    return this.send(
      {
        source: input.address,
        feeToken: input.token,
        maxSendStroops: input.maxSendStroops,
        maxSend: input.maxSend,
        ops: [
          { type: "create_account", destination: input.address },
          { type: "change_trust", asset: input.token },
          { type: "claim_balance", balance_id: input.balanceId },
        ],
      },
      sign,
    );
  }

  /**
   * The assets this deployment accepts as payment, with what is known about
   * each issuer. An asset code is not an identity on Stellar — always show the
   * issuer or its domain to the user, never the code alone.
   */
  async tokens(): Promise<{ tokens: string[]; known: KnownToken[] }> {
    const res = await this.doFetch(`${this.url}/v1/tokens`, { headers: this.headers });
    return (await res.json()) as { tokens: string[]; known: KnownToken[] };
  }
}
