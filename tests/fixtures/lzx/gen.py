"""LZX fixtures for primase-foundations, made by Windows' own LZX encoder: makecab (`/D CompressionType=LZX
/D CompressionMemory=<bits>`) packs `plain.bin` into a cabinet, and each cabinet data block (CFDATA: one frame, on a
bit stream of its own) is written out as `[u32 LE compressed size][u32 LE decoded size][the compressed bytes]`.
`window16.frames` is the 64 KiB window XNA's content uses, `window21.frames` the largest. `plain.bin` (about 200 KiB)
mixes repeated text, x86-like `E8` calls (which the encoder translates), and pseudo-random bytes.
usage: python -I gen.py OUTDIR (Windows: needs makecab)"""
import os, struct, subprocess, sys, tempfile

out = sys.argv[1]
parts = []
for i in range(400):
    parts.append(b"LZX keeps a window of the bytes it has made; a match copies from it. Line %d.\n" % i)
x = 12345
code = bytearray()
for i in range(6000):
    x = (x * 1103515245 + 12345) & 0x7FFFFFFF
    code += b"\x55\x8b\xec" + b"\xe8" + struct.pack("<i", (x % 40000) - 20000) + b"\x5d\xc3"
parts.append(bytes(code))
noise = bytearray()
for i in range(70000):
    x = (x * 1103515245 + 12345) & 0x7FFFFFFF
    noise.append(x >> 16 & 0xFF)
parts.append(bytes(noise))
parts.append(b"the end, short of a whole frame" * 11)
plain = b"".join(parts)
with open(os.path.join(out, "plain.bin"), "wb") as f:
    f.write(plain)

for bits in (16, 21):
    with tempfile.TemporaryDirectory() as tmp:
        src = os.path.join(tmp, "plain.bin")
        with open(src, "wb") as f:
            f.write(plain)
        cab = os.path.join(tmp, "plain.cab")
        subprocess.run(["makecab", "/D", "CompressionType=LZX", "/D", "CompressionMemory=%d" % bits, src, cab],
                       check=True, capture_output=True)
        b = open(cab, "rb").read()
    assert b[:4] == b"MSCF"
    flags = struct.unpack_from("<H", b, 30)[0]
    assert flags == 0, "a cabinet with reserved fields or a set"
    folders = struct.unpack_from("<H", b, 26)[0]
    assert folders == 1
    data_at, count, kind = struct.unpack_from("<IHH", b, 36)
    assert kind & 0xFF == 3 and kind >> 8 == bits, hex(kind)
    frames = bytearray()
    total = 0
    for _ in range(count):
        _, size, decoded = struct.unpack_from("<IHH", b, data_at)
        frames += struct.pack("<II", size, decoded) + b[data_at + 8:data_at + 8 + size]
        total += decoded
        data_at += 8 + size
    assert total == len(plain)
    with open(os.path.join(out, "window%d.frames" % bits), "wb") as f:
        f.write(frames)
    print(bits, count, "frames", len(frames), "bytes")
