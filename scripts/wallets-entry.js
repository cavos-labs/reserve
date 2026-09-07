// The wallet connector, bundled once and vendored so the page asks nothing of
// any CDN. Browser and extension wallets only: Ledger and Trezor need USB
// transports, and WalletConnect needs a project id, so neither belongs on a
// page whose whole job is handing out a key.
//
// Regenerate with:
//   npx esbuild swk-entry.js --bundle --format=iife --global-name=SWK --minify \
//     --outfile=crates/api/static/wallets.js
import { StellarWalletsKit, Networks } from "@creit.tech/stellar-wallets-kit";
import { AlbedoModule } from "@creit.tech/stellar-wallets-kit/modules/albedo";
import { FreighterModule } from "@creit.tech/stellar-wallets-kit/modules/freighter";
import { LobstrModule } from "@creit.tech/stellar-wallets-kit/modules/lobstr";
import { RabetModule } from "@creit.tech/stellar-wallets-kit/modules/rabet";
import { xBullModule } from "@creit.tech/stellar-wallets-kit/modules/xbull";

export { StellarWalletsKit, Networks };
export const modules = () => [
  new FreighterModule(),
  new xBullModule(),
  new AlbedoModule(),
  new LobstrModule(),
  new RabetModule(),
];
