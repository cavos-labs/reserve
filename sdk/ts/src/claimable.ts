import { Claimant, StrKey, xdr } from "@stellar/stellar-sdk";

/**
 * Seconds after creation before the sender can reclaim an unclaimed balance.
 * Keep in sync with `RECLAIM_AFTER_SECONDS` in the Rust service.
 */
export const RECLAIM_AFTER_SECONDS = 7 * 24 * 60 * 60;

export function reclaimPredicate(): xdr.ClaimPredicate {
  return Claimant.predicateNot(
    Claimant.predicateBeforeRelativeTime(String(RECLAIM_AFTER_SECONDS)),
  );
}

/** Destination first in intent; stellar-core requires the pair sorted by account id. */
export function claimableClaimants(source: string, destination: string): Claimant[] {
  const dest = new Claimant(destination, Claimant.predicateUnconditional());
  const reclaim = new Claimant(source, reclaimPredicate());
  return [dest, reclaim].sort((a, b) => compareAccount(a.destination, b.destination));
}

export function samePredicate(got: xdr.ClaimPredicate, want: xdr.ClaimPredicate): boolean {
  return got.toXDR("base64") === want.toXDR("base64");
}

function compareAccount(a: string, b: string): number {
  const left = StrKey.decodeEd25519PublicKey(a);
  const right = StrKey.decodeEd25519PublicKey(b);
  for (let i = 0; i < left.length; i++) {
    if (left[i] !== right[i]) return left[i]! - right[i]!;
  }
  return 0;
}
