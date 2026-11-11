import { Fragment } from "react";

/** `ab12…` -> `AB:12:…`, the form fingerprints are compared in; lines break
 * only between pairs so no pair is ever split across lines. */
export function Fingerprint({ sha256 }: { sha256: string }) {
  const pairs = sha256.toUpperCase().match(/.{2}/g) ?? [sha256];
  return (
    <code className="fingerprint">
      {pairs.map((pair, index) => (
        <Fragment key={index}>
          {pair}
          {index < pairs.length - 1 && (
            <>
              :<wbr />
            </>
          )}
        </Fragment>
      ))}
    </code>
  );
}
