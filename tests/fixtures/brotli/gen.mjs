// Brotli fixture for primase-foundations: `plain.txt` compressed by Node's zlib (Node 24.21.0), which is Google's
// Brotli library, at quality 11 with a 64 KiB window, to `plain.br`.
// usage: node gen.mjs OUTDIR
import { brotliCompressSync, constants } from "node:zlib";
import { writeFileSync } from "node:fs";
import { join } from "node:path";

const out = process.argv[2];
const plain = Buffer.from("AssetRipper packs its type trees with Brotli. ".repeat(8));
writeFileSync(join(out, "plain.txt"), plain);
const p = constants;
writeFileSync(
  join(out, "plain.br"),
  brotliCompressSync(plain, {
    params: {
      [p.BROTLI_PARAM_QUALITY]: 11,
      [p.BROTLI_PARAM_LGWIN]: 16,
      [p.BROTLI_PARAM_SIZE_HINT]: plain.length,
    },
  }),
);
