"""LZMA fixtures for primase-foundations, in the form Unity's asset bundles and 7-Zip store LZMA: the five property
bytes and the raw stream, no 8-byte size. `no-marker.lzma` is cut from a .7z made by p7zip 16.02 (`7z a -t7z
-m0=LZMA:d=64k -mhc=off`: the packed stream and the coder's properties from the uncompressed header), which ends the
stream without an end-of-payload marker as Unity does; `marker.lzma` is CPython's liblzma `.lzma` (FORMAT_ALONE)
output with its 8-byte size taken out, which ends with the marker. Both decode to `plain.txt`.
usage: python -I gen.py OUTDIR (needs 7z on PATH)"""
import lzma, os, struct, subprocess, sys, tempfile

out = sys.argv[1]
plain = b"Unity stores LZMA without an end marker; 7-Zip does too. " * 6
with open(os.path.join(out, "plain.txt"), "wb") as f:
    f.write(plain)
with tempfile.TemporaryDirectory() as tmp:
    src = os.path.join(tmp, "plain.txt")
    with open(src, "wb") as f:
        f.write(plain)
    archive = os.path.join(tmp, "a.7z")
    subprocess.run(["7z", "a", "-t7z", "-m0=LZMA:d=64k", "-mhc=off", archive, src], check=True, capture_output=True)
    b = open(archive, "rb").read()
next_offset, next_size = struct.unpack_from("<QQ", b, 12)
header = b[32 + next_offset:32 + next_offset + next_size]
coder = header.find(bytes([0x03, 0x01, 0x01]))  # the LZMA coder id, then the property count (5) and the properties
with open(os.path.join(out, "no-marker.lzma"), "wb") as f:
    f.write(header[coder + 4:coder + 9] + b[32:32 + next_offset])
alone = lzma.compress(plain, format=lzma.FORMAT_ALONE, filters=[{"id": lzma.FILTER_LZMA1, "dict_size": 65536}])
with open(os.path.join(out, "marker.lzma"), "wb") as f:
    f.write(alone[:5] + alone[13:])
