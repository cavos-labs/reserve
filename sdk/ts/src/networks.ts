/** Well-known Stellar passphrases. Kept here so the happy path does not import the SDK. */
export const TESTNET = "Test SDF Network ; September 2015";
export const PUBLIC = "Public Global Stellar Network ; September 2015";

export type NetworkName = "testnet" | "mainnet";

export const HOSTED = {
  testnet: {
    url: "https://reserve.cavos.xyz/testnet",
    networkPassphrase: TESTNET,
  },
  mainnet: {
    url: "https://reserve.cavos.xyz/mainnet",
    networkPassphrase: PUBLIC,
  },
} as const;

export function passphraseOf(network: NetworkName): string {
  return HOSTED[network].networkPassphrase;
}

export function networkOfPassphrase(passphrase: string): NetworkName | undefined {
  if (passphrase === TESTNET) return "testnet";
  if (passphrase === PUBLIC) return "mainnet";
  return undefined;
}

export function networkFromUrl(url: string): NetworkName | undefined {
  try {
    const path = new URL(url).pathname.replace(/\/+$/, "");
    if (path.endsWith("/mainnet")) return "mainnet";
    if (path.endsWith("/testnet")) return "testnet";
  } catch {
    // Not a URL. Ignore.
  }
  return undefined;
}
