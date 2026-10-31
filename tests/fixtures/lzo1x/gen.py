"""LZO1X fixtures for primase-foundations, made by python-lzo 1.15 (LZO 2.10's lzo1x_1 and lzo1x_999 compressors)
from synthetic inputs. usage: python -I gen.py OUTDIR"""
import hashlib, os, sys
import lzo

def lcg(n, seed, mask):
    s, out = seed, bytearray()
    for _ in range(n):
        s = (s * 1103515245 + 12345) & 0xFFFFFFFF
        out.append((s >> 16) & mask)
    return bytes(out)

def inputs():
    yield "text", b"".join(b"The quick brown fox jumps over the lazy dog %d. " % i for i in range(400))
    yield "low-entropy", lcg(16384, 1, 0x0F)
    far = lcg(24576, 7, 0xFF)
    yield "far", far + far + far[:1000]
    yield "zeros", bytes(70000)
    yield "one", b"a"
    yield "short", b"abcabcabcabcabcabcxyz"

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
with open(os.path.join(out, "manifest.tsv"), "w", newline="\n") as m:
    m.write("file\tinput\tlevel\tlength\tmd5\n")
    for name, data in inputs():
        for level in (1, 9):
            packed = lzo.compress(data, level, False)
            f = f"{name}-{level}.lzo1x"
            open(os.path.join(out, f), "wb").write(packed)
            assert lzo.decompress(packed, False, len(data)) == data
            m.write(f"{f}\t{name}\t{level}\t{len(data)}\t{hashlib.md5(data).hexdigest()}\n")
