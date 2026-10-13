// The current profile's custom CA (Plan.md P8.7, ADR-0017): how many certificates and
// their SHA-256 fingerprints, for the user to compare with what the server's admin gave
// them. Never the CA's text, which the UI never receives.
import type { CustomCaView } from "../../../generated/CustomCaView";

/** The current custom CA. */
export function CaDetails({ ca }: { ca: CustomCaView }) {
  return (
    <div className="flex flex-col gap-1 rounded-lg border p-3 text-sm">
      <p className="font-medium">
        {`Current custom CA: ${ca.certificates} ${ca.certificates === 1 ? "certificate" : "certificates"}`}
      </p>
      <ul aria-label="Certificate fingerprints" className="font-mono text-xs break-all">
        {ca.fingerprints.map((fingerprint) => (
          <li key={fingerprint}>{fingerprint}</li>
        ))}
      </ul>
      <p className="text-muted-foreground">
        Compare these with the fingerprints the server's admin gave you.
      </p>
    </div>
  );
}
