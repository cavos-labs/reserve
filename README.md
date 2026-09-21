# Cavos Reserve

Pay Stellar **reserves and fees with any token**. An account that holds nothing
but USDC — or any asset with a route to XLM — can be created, open trustlines
and transact, without ever touching a lumen.

Stellar needs XLM for two different things, and Reserve covers both:

| | what it is | how it is covered |
|---|---|---|
| **Reserves** | XLM *locked*: 1 XLM per account (2 × 0.5 base reserve) + 0.5 per subentry | sponsored reserves (CAP-33): the sponsor carries them, the user's minimum balance stays at 0 |
| **Fees** | XLM *spent*: 100 stroops per operation | fee-bump (CAP-15): the sponsor pays, the user's transaction is untouched |

The user pays for both in their own token, in the same transaction: a
`PathPaymentStrictReceive` buys exactly the XLM we are owed through the SDEX and
liquidity pools. No oracle — the rate is the route the payment itself takes. If
liquidity moves beyond the quoted `sendMax`, the whole transaction fails and the
user is charged nothing.

## Not custodial

- Reserve never holds user keys. It signs only the **outer** fee-bump envelope,
  which cannot alter the inner transaction.
- Sponsorship is a reserve obligation, not authority: the service never adds
  itself as a signer and never touches `setOptions`. The request type simply
  cannot express it.
- Reclaiming reserves cannot brick an account — `RevokeSponsorship` fails with
  `REVOKE_SPONSORSHIP_LOW_RESERVE` rather than stripping anything.
- The remaining risk is blind signing of a server-built XDR, which is why the
  client SDK must rebuild and verify the transaction locally before signing.

## API

```
POST /{network}/v1/quote          { source, fee_token, ops[] }    -> signed quote + price
POST /{network}/v1/build          { quote }                       -> unsigned inner tx XDR
POST /{network}/v1/submit         { quote, signed_xdr }           -> hash
POST /{network}/v1/challenge      { address }                     -> prove an address is yours
POST /{network}/v1/keys           { signed_xdr }                  -> a key, for a bigger budget
GET  /{network}/v1/tokens  /{network}/v1/status/{hash}
GET  /  (the product page)  /health  /{network}/health  /{network}/metrics
```

`network` is `testnet` or `mainnet`. Point the SDK at the prefix
(`https://reserve.cavos.xyz/testnet`). A process with one sponsor still
answers `/v1` at the root so local `RESERVE_URL=http://127.0.0.1:8080` works.

Quotes are **not stored**: they travel as an HMAC-signed payload, so the hot
path does no database writes. At submit time the service rebuilds the
transaction from the quote and compares XDR byte for byte — that comparison,
not an operation allowlist, is the security gate.

## Accepted tokens

Payment is only ever taken in an allowlisted asset, and the list is never
empty. An asset code means nothing on its own: mainnet carries **430 different
issuers of something called "USDC"** and 149 of "SHX", so a service that
accepted whatever a caller named would accept worthless look-alikes as payment.

The default mainnet list, verified on chain — dominant issuer by holders, home
domain declared by the issuing account, and a direct SDEX route to XLM without
which the fee cannot settle:

| asset | issuer domain | slippage | why |
|---|---|---|---|
| XLM | stellar.org | 0 | the native asset; nothing is converted |
| USDC | circle.com | 100 bps | 2.4M holders, ~$271M — the default on Stellar |
| USDT0 | usdt0.to | 500 bps | how Tether's USDT exists here: a LayerZero OFT, not a native issuance |
| EURC | circle.com | 100 bps | Circle's euro stablecoin, 33k holders |
| PYUSD | token-metadata.paxos.com | 100 bps | PayPal USD via Paxos, 9.7k holders |
| USDGLO | app.glodollar.org | 500 bps | small, but a real stablecoin with a direct route |
| AQUA | aqua.network | 200 bps | 192k holders — the most held non-stablecoin |
| SHX | stronghold.co | 200 bps | 92k holders |
| yXLM | ultracapital.xyz | 50 bps | tracks XLM one to one, 54k holders |
| yUSDC | ultracapital.xyz | 150 bps | 35k holders, liquidity in pools |

Left out on purpose: BENJI, EUTBL and USTBL have real volume but strict-receive
path finding returns nothing for them — no route to XLM means the fee cannot be
collected, and they are permissioned besides.

## What it charges

Twenty per cent over cost, with a floor of `RESERVE_MIN_CHARGE_STROOPS`
(56,000 stroops, about $0.001 at XLM near $0.18). The floor is the actual
price: a margin on Stellar's network fee is a margin on $0.000006, which
rounds to nothing. What a user pays for here is not a network fee, it is not
having to hold XLM at all.

Reserves are passed on in full and never discounted — they are money handed
over, not a service. A new account with one trustline costs 1.5 XLM of
reserves, so it is charged 1.5 XLM plus margin, out of the funds it claims.

The floor is set in stroops rather than dollars deliberately: no price feed, no
hidden dependency. It is worth revisiting if XLM moves a long way.

## Rate limits

Two different things need protecting, and they need different limits.

The upstream quota is the fragile one. Quoting is free for the caller and
costly for us: every quote runs path finding against Horizon. A loop of
unsigned quote requests never touches a key, never spends a stroop, and can
still exhaust the rate limit the whole service shares. It gets the tightest
limit, per calling address, with a global ceiling behind it.

The sponsor's XLM was the other candidate, and it did not survive the
arithmetic. A failed submission costs the fee bump, about 600 stroops, and the
attacker needs a funded account and a signed transaction for each one. Burning
ten dollars of ours takes a million transactions, which the limits above
already throttle. So there is no separate breaker: the money is defended by the
same limit that defends everything else, and by watching a balance that only
falls when something is wrong.

### Integrator keys

A key is an identity, never an authorisation. Unkeyed callers are served too,
from a smaller budget, so "paste the URL and it works" stays true and nobody
has to ask permission to try the service. Nobody is billed for a key either.

Keys are signed by whoever issues them and verified here **offline**, with two
consequences on purpose:

- the hot path never calls the issuer, so issuing being down cannot take this
  service down, and a self-hosted deployment needs nothing from anybody;
- the signature is **asymmetric**. This service holds only the public half, so
  compromising it, or any self-hosted copy, does not let anyone mint keys. A
  shared secret would have.

Getting one takes a Stellar wallet and no account. `GET /` is the whole
interface: connect, sign a challenge, keep the key.

```
POST /v1/challenge  { address }     -> a transaction to sign
POST /v1/keys       { signed_xdr }  -> cav_…
```

The challenge is shaped after SEP-10: sequence number zero, so the network can
never accept it, and one `ManageData` operation sourced by the account being
proved. Every wallet can sign a transaction, including through WalletConnect,
which exposes only `stellar_signXDR`; message signing is not available
everywhere. It carries its own MAC, so no nonces are stored.

Nothing about the key is stored either. It is a signed statement about an
address, so signing the challenge again hands back the same key: there is no
account, no session, and nothing to recover.

Set `RESERVE_KEY_ISSUER_SECRET` where keys are handed out;
`RESERVE_KEY_ISSUER_PUBKEY` alone is enough to accept them, which is what a
self-hosted copy wants. `scripts/mint-key.mjs` mints one from the command line
for cases with no browser.

Worth being plain about what this login is not. Stellar addresses are free, so
proving one gives identity, not scarcity: nothing stops somebody generating a
thousand and asking for a thousand keys. The global limit is what actually
protects the service, and per-key budgets divide it rather than enlarge it.
There is no revocation list either, for the same reason: a leaked key buys a
larger rate-limit bucket and nothing else.

`RESERVE_TRUST_PROXY` must stay false unless a proxy in front sets
`X-Forwarded-For`. Believing that header without one lets a caller pick their
own rate-limit bucket, which is the same as having no limit.

## Who carries the slippage

The fee payment is *strict receive*: the sponsor is credited exactly the quoted
XLM, and the user's balance is debited whatever the market asks, up to
`sendMax`. So the price risk between quote and submit is the user's — capped,
visible before signing, and checked by the SDK, which refuses to sign a
`sendMax` other than the quoted one. When the market moves in their favour they
pay less; `sendMax` is a ceiling, not a price.

Past that ceiling the whole inner transaction fails, the user pays nothing, and
**the sponsor is still charged the fee-bump fee** — the fee account is debited
even when the inner transaction fails. The cost of a band set too tight lands on
the service, which is why the numbers above are rounded up rather than down.
They come from measuring each asset's price impact at 100 XLM — four orders of
magnitude above any fee charged here — plus room for XLM moving during a quote's
~60 second life. `RESERVE_SLIPPAGE_BPS` only applies to assets an operator has
added by hand, since there is no basis for guessing at their market.

One consequence of fees this small: the rate is never read at the size being
bought. Asking Horizon what buys 0.0000360 XLM returns rounding artefacts —
routes priced at a sixtieth of the real rate — so pricing reads the rate at one
lumen and scales down, rounding up. Both the margin and the slippage band round
up too; at 65 stroops, 1% rounded down is nothing at all.

`RESERVE_TOKENS` overrides the list with explicit `CODE:ISSUER` entries. `*`
accepts anything with a route and is **refused on mainnet**; it is
there for development against throwaway assets. `GET /v1/tokens` returns the
live list with issuer and domain for each entry.

## Modes

- **sponsored** — the account exists, sources its own transaction, pays us in
  its token, and we fee-bump.
- **bootstrap** — the account does not exist yet, so it cannot source a
  transaction. The sponsor sources it and co-signs. It only works alongside
  money arriving: the request must include a `claim_balance` for funds someone
  left at that address, and the reserves and fees are paid out of what is
  claimed, in the same transaction.

### Why a new account has to arrive with money

Creating an empty account means handing over 1 XLM, plus 0.5 for each
trustline, that lives inside *the user's* account from then on. It comes back
only if they close the trustline or merge the account, and there is no way to
take it back: releasing a sponsored reserve requires the sponsored account to
cover it, and an empty one never can — `RevokeSponsorship` fails with
`REVOKE_SPONSORSHIP_LOW_RESERVE`. An address that is created and never used is
money gone for good.

So accounts are created at the moment they are funded, never before. Stellar
already has the mechanism for sending to someone who has no account: a
claimable balance. `pay` builds one when the destination cannot receive a
Payment — no account, or no trustline — and names the sender as a second
claimant so an unclaimed balance can be taken back after seven days. The
recipient's first transaction creates the account, opens the trustline, claims
the money and pays for all of it out of that money.

## Client SDK

`sdk/ts` — `@cavos/reserve`, wallet-agnostic:

```ts
const reserve = new Reserve({ url, networkPassphrase, sponsor });
const quote = await reserve.quote({
  source,
  feeToken: "USDC:GA5ZSEJ…",
  maxSendStroops,
  ops,
});
const { hash } = await reserve.send(quote, wallet.signTransaction);
```

The SDK keeps the request you made and refuses a quote — or a built
transaction — that does not match it, including a fee above `maxSendStroops`.
Comparing the built XDR only against the quote is not enough: a compromised
service can make those two agree. See `sdk/ts/README.md`.

## Infra

One static binary, one small VM, and no database at all. Fee-bumps do not
consume the sponsor's sequence, so **sponsored** submissions run in parallel
without extra accounts. Bootstrap does consume a sequence — the sponsor's,
unless `RESERVE_CHANNEL_SECRETS` names extra lanes. Without extras, only one
bootstrap quote is in flight at a time; a second caller gets `503 bootstrap_busy`
until the first submits or the quote's time bound passes.

There is nothing to persist. Quotes travel as signed payloads, so the hot path
writes nothing. How much XLM the sponsor has immobilised is answered by the
account's own `num_sponsoring` field, and a copy of that could only ever be
wrong. `GET /health` and `GET /metrics` report it, along with the spendable balance.

That balance is the number to alert on, and the useful alert is not a floor.
In normal operation it only rises: every transaction charges more than it
immobilises. So a balance that **falls for hours** means a bug, a fee token
that lost its route to XLM, or abuse, and it says so long before the service
runs dry and fails for everyone at once.

```bash
cp .env.example .env   # fill in the two secrets
docker compose up -d
```

or without Docker:

```bash
RESERVE_TESTNET_SPONSOR_SECRET=S... \
RESERVE_MAINNET_SPONSOR_SECRET=S... \
RESERVE_QUOTE_KEY=<32+ bytes> \
cargo run -p reserve-api
```

A single network still works: `RESERVE_NETWORK=testnet` plus
`RESERVE_SPONSOR_SECRET`.

| variable | default |
|---|---|
| `RESERVE_TESTNET_SPONSOR_SECRET` / `_FILE` | unset (enables `/testnet`) |
| `RESERVE_MAINNET_SPONSOR_SECRET` / `_FILE` | unset (enables `/mainnet`) |
| `RESERVE_TESTNET_HORIZON_URL` / `RESERVE_MAINNET_HORIZON_URL` | SDF Horizon for that network |
| `RESERVE_TESTNET_TOKENS` / `RESERVE_MAINNET_TOKENS` | curated list for that network |
| `RESERVE_TESTNET_CHANNEL_SECRETS` / `RESERVE_MAINNET_CHANNEL_SECRETS` | unset |
| `RESERVE_NETWORK` | `testnet` (legacy single-lane, with `RESERVE_SPONSOR_SECRET`) |
| `RESERVE_HORIZON_URL` | SDF Horizon (legacy single-lane) |
| `RESERVE_BIND` | `0.0.0.0:8080` |
| `RESERVE_MARGIN_BPS` | `2000` |
| `RESERVE_MIN_CHARGE_STROOPS` | `56000` (~$0.001) |
| `RESERVE_SLIPPAGE_BPS` | `100` (only for assets outside the curated list) |
| `RESERVE_QUOTE_LEDGERS` | `12` |
| `RESERVE_RATE_ANON_PER_MIN` | `30` |
| `RESERVE_RATE_KEYED_PER_MIN` | `600` |
| `RESERVE_RATE_GLOBAL_PER_MIN` | `3000` |
| `RESERVE_KEY_ISSUER_PUBKEY` | unset (hex, 32 bytes; no keys accepted without it) |
| `RESERVE_KEY_ISSUER_SECRET` / `_FILE` | unset (set only where keys are issued) |
| `RESERVE_QUOTE_KEY` / `_FILE` | required, 32+ bytes |
| `RESERVE_TRUST_PROXY` | `false` |

## Tests

```bash
cargo test                       # unit
(cd sdk/ts && npm test)          # the client-side verifier
RESERVE_SPONSOR_SECRET=S... RESERVE_URL=http://127.0.0.1:8080 \
  cargo run -p reserve-e2e       # end-to-end against testnet
```

It needs `RESERVE_TOKENS=*` on the service, since it mints its own throwaway
asset. The run stands up an issuer and a market maker on testnet, creates a
zero-XLM account through the API, funds it with the token only, makes it pay for
its own transaction, and checks that a tampered submission is rejected.
