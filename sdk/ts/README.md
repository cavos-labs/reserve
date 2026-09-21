# @cavos/reserve

Pay Stellar reserves and fees with any token, from any wallet.

```bash
npm install @cavos/reserve @stellar/stellar-sdk
```

```ts
import { Reserve } from "@cavos/reserve";

const reserve = new Reserve("testnet");
// Mainnet: new Reserve("mainnet")
// Pin the sponsor from /health: await Reserve.connect("testnet")

const { hash } = await reserve.pay({
  source: address,
  destination,
  amount: "10",
  token: "USDC:GA5ZSEJ…",
  maxSend: "0.05",
}, (xdr, { networkPassphrase }) =>
  wallet.signTransaction(xdr, { networkPassphrase }),
);
```

The signer is whatever your wallet already exposes — Freighter, Albedo, a bare
`Keypair`. Nothing in this package is specific to any wallet or to any account
type: any `G…` address works.

`pay` looks at the destination. If it can receive a Payment, that is what is
built. If there is no account, or no trustline, the money is left as a
claimable balance the recipient opens with `activate`. The sender stays a
claimant: if nobody claims it, they can take it back after seven days.

## Verification is the point

The service builds the bytes a wallet is asked to sign. Comparing those bytes
only against the quote the service just sealed proves nothing: a compromised
server can make the two agree. `quote` therefore keeps the request *you* made,
and `build` / `send` refuse anything that does not match it.

That check needs three things the request used to omit:

- **`maxSendStroops`** — the fee payment is not one of `ops`, so without a
  ceiling a quote for any amount at all would verify cleanly.
- **the operations and fee token you asked for** — not the ones the payload
  claims you asked for.
- **`networkPassphrase` and `sponsor`**, when you set them on the client —
  neither is inside the envelope a wallet signs.

The path of the fee payment is compared hop for hop against the quote. A
hostile route cannot spend more than `maxSendStroops`; that ceiling, not a
local path finder, is what bounds the loss.

Calling the HTTP endpoints directly gives this up. Use `send`, or call
`verifyTransaction` yourself with the request you made:

```ts
import { verifyTransaction, readQuote } from "@cavos/reserve";

verifyTransaction(xdr, readQuote(quoteToken), {
  source,
  feeToken: "USDC:GA5ZSEJ…",
  maxSendStroops: 500_000,
  ops,
});
```

## Onboarding someone with no account

An account is created in the same transaction that funds it — never emptily,
because reserves handed to an address that never comes back cannot be
recovered. Stellar's mechanism for paying someone who has no account yet is a
claimable balance; the recipient's first transaction does everything at once:

```ts
await reserve.activate({
  address: newAddress,
  token: "USDC:GA5ZSEJ…",
  balanceId,
  maxSend: "5",
}, sign);
```

The reserves (1.5 XLM for an account with one trustline) and the fees come out
of the claimed funds. `npm run example` does exactly this against testnet.

## Accepted tokens

```ts
const { known } = await reserve.tokens();
// [{ asset: "USDC:GA5ZSEJ…", code: "USDC", domain: "circle.com", … }]
```

Only allowlisted assets are accepted as payment. Show the issuer or its domain
in your UI, never the code alone: mainnet has hundreds of accounts issuing
something called "USDC".

## API

| | |
|---|---|
| `new Reserve("testnet" \| "mainnet")` | hosted URL + pinned passphrase |
| `Reserve.connect(network)` | same, plus sponsor pinned from `/health` |
| `pay({ source, destination, amount, token, maxSend })` | Payment, or a claimable if the dest is not ready |
| `activate({ address, token, balanceId, maxSend })` | create + trust + claim |
| `quote({ source, ops, feeToken?, maxSend })` | price classic operations |
| `send(quote \| request, signer)` | build → verify → sign → submit |
| `build(quote)` | fetch the transaction and verify it against the held request |
| `submit(quote, signedXdr)` | submit an already-signed transaction |
| `tokens()` | the assets this deployment accepts, with issuer and domain |
| `readQuote(token)` | decode a quote payload |
| `verifyQuote(payload, request, expected?)` | is this quote an answer to the request |
| `verifyTransaction(xdr, payload, request, expected?)` | the check, on its own |

Run `npm run example` against a local service to create an account that holds no
XLM at all.
