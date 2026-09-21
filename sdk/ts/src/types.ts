/** What the caller asks Reserve to sponsor. Mirrors the service's own allowlist. */
export type ReserveOp =
  | { type: "create_account"; destination: string }
  | { type: "payment"; destination: string; asset: string; amount: string }
  | { type: "change_trust"; asset: string; limit?: string }
  /**
   * Classic DEX swap. `send_amount` of `send_asset` leaves; at least `dest_min`
   * of `dest_asset` arrives. `path` is the hops in between, if any.
   */
  | {
      type: "path_payment_strict_send";
      destination: string;
      send_asset: string;
      send_amount: string;
      dest_asset: string;
      dest_min: string;
      path?: string[];
    }
    /**
     * Claim money left for this address. An account that does not exist yet is
     * only ever created alongside one of these: the reserves it needs are paid
     * for out of the funds arriving, not given away.
     */
    | { type: "claim_balance"; balance_id: string }
    /**
     * Leave money for `destination` when a Payment would fail. The service
     * always adds the sender as a second claimant, reclaimable after seven days.
     */
    | { type: "create_claimable_balance"; destination: string; asset: string; amount: string };

/**
 * What the caller asked for, and the most they are willing to spend on it.
 *
 * Verification compares the service's answer against this. It must be built
 * from the caller's own inputs and never from anything the service returned:
 * a quote checked against the quote's own contents proves nothing.
 */
export interface ReserveRequest {
  /** The account the operations are for. */
  source: string;
  /** Canonical asset the fee is taken in. */
  feeToken: string;
  /**
   * Ceiling on what may leave the user's balance, in the fee token's stroops.
   *
   * This is the only bound on the price. The operations the caller lists do not
   * include the fee payment, so comparing operations cannot cap it — without
   * this, a quote for any amount at all verifies cleanly.
   */
  maxSendStroops: number | string;
  ops: ReserveOp[];
}

/**
 * Values the caller knows out of band, and will not take the service's word
 * for. Both are optional because not every caller knows them; anything left
 * unset is simply not checked.
 */
export interface ReserveExpectations {
  /** Refuse a quote for any other network. */
  networkPassphrase?: string;
  /** Refuse a quote that pays the fee to any other account. */
  sponsor?: string;
}

export type Mode = "sponsored" | "bootstrap";

/** The payload the service sealed. Readable by anyone; only the service can mint one. */
export interface QuotePayload {
  network: string;
  mode: Mode;
  source: string;
  sponsor: string;
  /**
   * Transaction source in bootstrap. Absent (or empty) means the sponsor, which
   * is the single-lane default. A distinct address is a channel account.
   */
  channel?: string;
  ops: ReserveOp[];
  fee_token: string;
  charge_stroops: number;
  send_max_stroops: number;
  path: string[];
  reserve_stroops: number;
  sequence: string;
  inner_fee_stroops: number;
  min_time: number;
  max_time: number;
  expires_at_ledger: number;
}

export interface Quote {
  /** Opaque token to hand back to `build` and `submit`. */
  token: string;
  payload: QuotePayload;
  /**
   * The request this quote answers, as the caller stated it. Kept because
   * `payload` is the service's account of what was asked, and the two have to
   * be compared before anything is signed.
   */
  request: ReserveRequest;
  mode: Mode;
  /** XLM the service receives, in stroops. */
  chargeStroops: number;
  /** Cap on what leaves the user's balance, in the fee token's units. */
  sendMaxStroops: number;
  /** XLM this locks up in reserves on the service's side. */
  reserveStroops: number;
  /**
   * Head-room applied over the quoted price, in basis points. `sendMaxStroops`
   * is the worst case; a strict-receive payment spends only what the market
   * asks, so the user usually pays less.
   */
  slippageBps: number;
  /** True when this transaction also creates the account. */
  createsAccount: boolean;
  expiresAtLedger: number;
}

/**
 * Signs a transaction envelope and returns the signed XDR.
 *
 * Deliberately the same shape wallets already expose (Freighter, Albedo, a bare
 * `Keypair`), so no wallet has to integrate anything specific to Reserve.
 */
export type Signer = (
  xdr: string,
  opts: { networkPassphrase: string },
) => Promise<string> | string;

export interface SubmitResult {
  hash: string;
  ledger?: number;
}

/** An asset the service accepts as payment. */
export interface KnownToken {
  /** Canonical `CODE:ISSUER`, or `native`. */
  asset: string;
  code: string;
  /** Empty for the native asset. */
  issuer: string;
  /** The `home_domain` the issuing account declares on chain. */
  domain: string;
  /** Head-room this asset gets over the quoted price, in basis points. */
  slippageBps?: number;
  note: string;
}
