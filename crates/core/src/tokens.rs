//! The tokens Reserve accepts as payment.
//!
//! This is an allowlist, not a filter, and it is never empty: an asset code on
//! Stellar means nothing on its own. Mainnet currently carries 430 different
//! issuers of something called "USDC" and 149 of "SHX", so accepting whatever
//! a caller names would mean accepting worthless look-alikes as payment.
//!
//! Every entry below was checked against mainnet on 2026-09-04:
//! the issuer is the dominant one by holders, it declares the home domain of
//! the organisation it claims to belong to, and it has a direct (zero-hop)
//! route to XLM on the SDEX — without that route the fee cannot be collected.

use crate::asset::Asset;

/// An asset this service is willing to be paid in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownToken {
    pub code: &'static str,
    /// Empty for the native asset.
    pub issuer: &'static str,
    /// The `home_domain` the issuing account declares on chain.
    pub domain: &'static str,
    /// Head-room over the quoted price for this asset, in basis points.
    ///
    /// A band that is too tight costs *us*: the path payment fails, the whole
    /// inner transaction fails with it, and the fee bump is charged to the
    /// sponsor anyway. Too wide costs the *user*, but only in the worst case
    /// and only up to this much — strict-receive spends what the market asks
    /// and no more. So the numbers below are set from measured behaviour and
    /// then rounded up, not down.
    ///
    /// Basis for each value (mainnet, 2026-09-04): price impact of buying
    /// 100 XLM with the asset versus buying 1 XLM — four orders of magnitude
    /// above any fee this service charges — plus room for XLM moving during
    /// the ~60 second life of a quote.
    pub slippage_bps: u32,
    pub note: &'static str,
}

impl KnownToken {
    pub fn canonical(&self) -> String {
        if self.issuer.is_empty() {
            "native".to_string()
        } else {
            format!("{}:{}", self.code, self.issuer)
        }
    }
}

pub const PUBLIC_NETWORK: &str = "Public Global Stellar Network ; September 2015";
pub const TESTNET: &str = "Test SDF Network ; September 2015";

/// Mainnet allowlist, most used first.
///
/// Holder counts are authorised trustlines at the time of the check; they are
/// what justifies each entry being here rather than a guess about relevance.
pub const MAINNET_TOKENS: &[KnownToken] = &[
    KnownToken {
        code: "XLM",
        issuer: "",
        domain: "stellar.org",
        // Nothing is converted: the path payment is XLM for XLM.
        slippage_bps: 0,
        note: "the native asset; no conversion needed",
    },
    KnownToken {
        code: "USDC",
        issuer: "GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN",
        domain: "circle.com",
        // 8 bps spread, 3 bps impact at 100 XLM. The band is almost all
        // volatility allowance.
        slippage_bps: 100,
        note: "Circle. 2.4M holders, ~$271M outstanding — the default on Stellar",
    },
    KnownToken {
        code: "USDT0",
        issuer: "GATISXX6BZ6NC7IKQBY37CJD4SOZL3CYZJWXEDG6JVIY4WBS6KXJHN6Q",
        // The issuing account declares no home_domain; the address is the one
        // published by usdt0.to and in the USDT0 deployment docs.
        domain: "usdt0.to",
        // Its price through path finding tracks USDC, but the liquidity is in
        // pools rather than the order book: 31% book spread and 95 bps impact
        // at 100 XLM. Widest band of the stablecoins.
        slippage_bps: 500,
        note: "how Tether's USDT exists on Stellar: a LayerZero OFT, not a native Tether issuance",
    },
    KnownToken {
        code: "EURC",
        issuer: "GDHU6WRG4IEQXM5NZ4BMPKOXHW76MZM4Y2IEMFDVXBSDP6SJY4ITNPP2",
        domain: "circle.com",
        // 27 bps spread, no measurable impact at 100 XLM.
        slippage_bps: 100,
        note: "Circle's euro stablecoin. 33k holders",
    },
    KnownToken {
        code: "PYUSD",
        issuer: "GDQE7IXJ4HUHV6RQHIUPRJSEZE4DRS5WY577O2FY6YQ5LVWZ7JZTU2V5",
        domain: "token-metadata.paxos.com",
        // 30 bps spread, 2 bps impact at 100 XLM.
        slippage_bps: 100,
        note: "PayPal USD, issued by Paxos. 9.7k holders",
    },
    KnownToken {
        code: "USDGLO",
        issuer: "GBBS25EGYQPGEZCGCFBKG4OAGFXU6DSOQBGTHELLJT3HZXZJ34HWS6XV",
        domain: "app.glodollar.org",
        // 65% book spread and 51 bps impact at 100 XLM — thin, so the band
        // has to be generous or its transactions simply fail.
        slippage_bps: 500,
        note: "Glo Dollar. Small, but a real stablecoin with a direct route",
    },
    KnownToken {
        code: "AQUA",
        issuer: "GBNZILSTVQZ4R7IKQDGHYGY2QXL5QOFJYQMXPKWRRM5PAV7Y4M67AQUA",
        domain: "aqua.network",
        // 30 bps spread, 6 bps impact, but a volatile asset against XLM.
        slippage_bps: 200,
        note: "Aquarius. 192k holders — the most held non-stablecoin on Stellar",
    },
    KnownToken {
        code: "SHX",
        issuer: "GDSTRSHXHGJ7ZIVRBXEYE5Q74XUVCUSEKEBR7UCHEUUEK72N7I7KJ6JH",
        domain: "stronghold.co",
        // 40 bps spread, 7 bps impact; volatile against XLM.
        slippage_bps: 200,
        note: "Stronghold. 92k holders",
    },
    KnownToken {
        code: "yXLM",
        issuer: "GARDNV3Q7YGT4AKSDF25LT32YSCCW4EV22Y2TV3I2PU2MMXJTEDL5T55",
        domain: "ultracapital.xyz",
        // Tracks XLM one to one: 9 bps spread, no measurable impact.
        slippage_bps: 50,
        note: "Ultra Capital's yield-bearing XLM. 54k holders",
    },
    KnownToken {
        code: "yUSDC",
        issuer: "GDGTVWSM4MGS4T7Z6W4RPWOCHE2I6RDFCIFZGS3DOA63LWQTRNZNTTFF",
        domain: "ultracapital.xyz",
        // Liquidity sits in pools rather than the order book; 3 bps impact.
        slippage_bps: 150,
        note: "Ultra Capital's yield-bearing USDC. 35k holders",
    },
];

// Deliberately excluded, and why:
//
// * BENJI (Franklin Templeton), EUTBL / USTBL (Spiko treasury bills) — real
//   assets with real volume, but strict-receive path finding returns nothing
//   for them: no SDEX route to XLM, so a fee payment in them cannot settle.
//   They are also permissioned, which makes a relayer a poor holder of them.
// * Everything else sharing these codes. The impostors are the reason this
//   list exists.

/// Testnet has no meaningful market, so nothing is allowlisted by default
/// beyond the native asset; a deployment there is expected to name its own.
pub const TESTNET_TOKENS: &[KnownToken] = &[KnownToken {
    code: "XLM",
    issuer: "",
    domain: "stellar.org",
    slippage_bps: 0,
    note: "the native asset",
}];

/// Look up a curated entry by its canonical asset string.
pub fn find(asset: &Asset, network_passphrase: &str) -> Option<&'static KnownToken> {
    let canonical = asset.canonical();
    known_tokens(network_passphrase)
        .iter()
        .find(|t| t.canonical() == canonical)
}

/// Head-room to allow over the quoted price for this asset.
///
/// Curated assets carry their own, measured value; anything an operator has
/// added by hand falls back to the deployment-wide default, since we have no
/// basis for guessing at its market.
pub fn slippage_bps(asset: &Asset, network_passphrase: &str, fallback: u32) -> u32 {
    find(asset, network_passphrase).map_or(fallback, |t| t.slippage_bps)
}

pub fn known_tokens(network_passphrase: &str) -> &'static [KnownToken] {
    match network_passphrase {
        PUBLIC_NETWORK => MAINNET_TOKENS,
        _ => TESTNET_TOKENS,
    }
}

/// What a deployment accepts, and how it was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allowlist {
    /// A fixed set of canonical asset strings.
    Only(Vec<String>),
    /// Every asset with a route to XLM. Only ever set on purpose, for
    /// development against throwaway assets — never on mainnet.
    Any,
}

impl Allowlist {
    /// Parse the operator's configuration. An empty value means "use the
    /// curated list for this network"; `*` opts out of the list entirely.
    pub fn parse(configured: &str, network_passphrase: &str) -> Allowlist {
        let configured = configured.trim();
        if configured == "*" {
            return Allowlist::Any;
        }
        let explicit: Vec<String> = configured
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if explicit.is_empty() {
            return Allowlist::Only(
                known_tokens(network_passphrase)
                    .iter()
                    .map(KnownToken::canonical)
                    .collect(),
            );
        }
        Allowlist::Only(explicit)
    }

    pub fn allows(&self, asset: &Asset) -> bool {
        match self {
            Allowlist::Any => true,
            Allowlist::Only(list) => {
                let canonical = asset.canonical();
                list.iter().any(|entry| {
                    entry == &canonical
                        // "XLM" reads better in configuration than "native".
                        || (canonical == "native" && (entry == "XLM" || entry == "native"))
                })
            }
        }
    }

    pub fn entries(&self) -> Vec<String> {
        match self {
            Allowlist::Any => vec!["*".to_string()],
            Allowlist::Only(list) => list.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::parse_asset;

    const REAL_USDC: &str = "USDC:GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN";
    // One of the 430 other accounts issuing something called USDC.
    const FAKE_USDC: &str = "USDC:GA24LJXFG73JGARIBG2GP6V5TNUUOS6BD23KOFCW3INLDY5KPKS7GACZ";

    #[test]
    fn the_default_list_is_the_curated_one() {
        let list = Allowlist::parse("", PUBLIC_NETWORK);
        assert!(list.allows(&parse_asset(REAL_USDC).unwrap()));
        assert!(list.allows(&parse_asset("native").unwrap()));
        assert_eq!(list.entries().len(), MAINNET_TOKENS.len());
    }

    #[test]
    fn a_look_alike_issuer_is_not_the_asset() {
        let list = Allowlist::parse("", PUBLIC_NETWORK);
        assert!(!list.allows(&parse_asset(FAKE_USDC).unwrap()));
    }

    #[test]
    fn an_explicit_list_replaces_the_defaults() {
        let list = Allowlist::parse(&format!(" {REAL_USDC} , XLM "), PUBLIC_NETWORK);
        assert!(list.allows(&parse_asset(REAL_USDC).unwrap()));
        assert!(list.allows(&parse_asset("native").unwrap()));
        assert!(!list.allows(
            &parse_asset("AQUA:GBNZILSTVQZ4R7IKQDGHYGY2QXL5QOFJYQMXPKWRRM5PAV7Y4M67AQUA").unwrap()
        ));
    }

    #[test]
    fn the_wildcard_has_to_be_asked_for() {
        assert_eq!(Allowlist::parse("*", TESTNET), Allowlist::Any);
        assert!(Allowlist::parse("*", TESTNET).allows(&parse_asset(FAKE_USDC).unwrap()));
        // Absence of configuration never becomes a wildcard.
        assert!(matches!(Allowlist::parse("", TESTNET), Allowlist::Only(_)));
    }

    #[test]
    fn testnet_defaults_to_the_native_asset_only() {
        let list = Allowlist::parse("", TESTNET);
        assert!(list.allows(&parse_asset("native").unwrap()));
        assert!(!list.allows(&parse_asset(REAL_USDC).unwrap()));
    }

    #[test]
    fn slippage_comes_from_the_asset_not_the_deployment() {
        let usdc = parse_asset(REAL_USDC).unwrap();
        assert_eq!(slippage_bps(&usdc, PUBLIC_NETWORK, 9_999), 100);

        // Converting XLM to XLM cannot slip.
        assert_eq!(
            slippage_bps(&parse_asset("native").unwrap(), PUBLIC_NETWORK, 9_999),
            0
        );

        // Thin books get a wider band, or their transactions just fail.
        let glo =
            parse_asset("USDGLO:GBBS25EGYQPGEZCGCFBKG4OAGFXU6DSOQBGTHELLJT3HZXZJ34HWS6XV").unwrap();
        assert_eq!(slippage_bps(&glo, PUBLIC_NETWORK, 9_999), 500);

        // An asset we know nothing about uses the deployment default.
        assert_eq!(
            slippage_bps(&parse_asset(FAKE_USDC).unwrap(), PUBLIC_NETWORK, 250),
            250
        );
    }

    #[test]
    fn every_curated_issuer_is_a_valid_account() {
        for token in MAINNET_TOKENS.iter().chain(TESTNET_TOKENS) {
            let asset =
                parse_asset(&token.canonical()).unwrap_or_else(|e| panic!("{}: {e}", token.code));
            assert!(!token.domain.is_empty(), "{} has no domain", token.code);
            // A band this wide would mean we do not understand the market.
            assert!(
                token.slippage_bps <= 1_000,
                "{} allows too much slippage",
                token.code
            );
            if token.issuer.is_empty() {
                assert_eq!(asset, Asset::Native);
            } else {
                crate::asset::account_id(token.issuer)
                    .unwrap_or_else(|e| panic!("{}: {e}", token.code));
            }
        }
    }
}
