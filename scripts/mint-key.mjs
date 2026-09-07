// Mints an integrator key, and doubles as the reference for whoever issues
// them. This is the whole of the issuing side: sign a small JSON payload with
// an Ed25519 private key. The service holds only the public half and verifies
// offline, so issuing can be down without the service noticing.
//
//   node scripts/mint-key.mjs --new-issuer
//   node scripts/mint-key.mjs --issuer <hex private> --id acme --days 90
import { createPrivateKey, generateKeyPairSync, sign } from "node:crypto";

const argv = process.argv.slice(2);
const args = {};
for (let i = 0; i < argv.length; i++) {
  if (!argv[i].startsWith("--")) continue;
  const next = argv[i + 1];
  const hasValue = next !== undefined && !next.startsWith("--");
  args[argv[i].slice(2)] = hasValue ? next : true;
  if (hasValue) i++;
}

// Ed25519 keys are 32 raw bytes; node wants them wrapped in a PKCS#8 header.
const PKCS8_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");
const rawToPrivate = (hex) =>
  createPrivateKey({
    key: Buffer.concat([PKCS8_PREFIX, Buffer.from(hex, "hex")]),
    format: "der",
    type: "pkcs8",
  });

if (args["new-issuer"]) {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const priv = privateKey.export({ format: "der", type: "pkcs8" }).subarray(-32);
  const pub = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  console.log("private (keep this where keys are issued):", priv.toString("hex"));
  console.log("public  (RESERVE_KEY_ISSUER_PUBKEY):      ", pub.toString("hex"));
  process.exit(0);
}

const network =
  args.network === "public"
    ? "Public Global Stellar Network ; September 2015"
    : "Test SDF Network ; September 2015";

const payload = Buffer.from(
  JSON.stringify({
    id: args.id ?? "unnamed",
    network,
    ...(args.days ? { expires_at: Math.floor(Date.now() / 1000) + Number(args.days) * 86400 } : {}),
  }),
);
const signature = sign(null, payload, rawToPrivate(args.issuer));
console.log(`cav_${payload.toString("base64url")}.${signature.toString("base64url")}`);
