"""Print the ABC constant-pool strings of a SWF (all DoABC tags). usage: swf_strings.py file.swf"""
import sys, zlib, struct
data = open(sys.argv[1], 'rb').read()
sig = data[:3]
if sig == b'CWS':
    body = zlib.decompress(data[8:])
elif sig == b'FWS':
    body = data[8:]
else:
    sys.exit(f"unsupported {sig}")
# skip RECT + frame rate + count
nbits = body[0] >> 3
rect_bytes = (5 + 4 * nbits + 7) // 8
at = rect_bytes + 4
def u30(b, i):
    r = 0; shift = 0
    while True:
        c = b[i]; i += 1
        r |= (c & 0x7f) << shift; shift += 7
        if not c & 0x80 or shift >= 35: return r, i
out = []
while at < len(body):
    code_len = struct.unpack_from('<H', body, at)[0]; at += 2
    code, length = code_len >> 6, code_len & 0x3f
    if length == 0x3f:
        length = struct.unpack_from('<I', body, at)[0]; at += 4
    tag = body[at:at + length]; at += length
    if code in (72, 82):
        i = 0
        if code == 82:
            i = 4; i = tag.index(b'\0', i) + 1
        i += 4  # minor, major
        n, i = u30(tag, i)          # ints
        for _ in range(max(n - 1, 0)): _, i = u30(tag, i)
        n, i = u30(tag, i)          # uints
        for _ in range(max(n - 1, 0)): _, i = u30(tag, i)
        n, i = u30(tag, i); i += 8 * max(n - 1, 0)  # doubles
        n, i = u30(tag, i)
        for _ in range(max(n - 1, 0)):
            ln, i = u30(tag, i); out.append(tag[i:i + ln].decode('utf-8', 'replace')); i += ln
    if code == 0: break
print("\n".join(out))
