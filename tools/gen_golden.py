"""Generate golden dot statistics from the original Python app, for Rust parity tests.

usage: python3 -I tools/gen_golden.py /path/to/retroimpact_src_root /path/to/rustydotmatrix
"""
import json, os, random, sys, math
src, repo = sys.argv[1], sys.argv[2]
sys.path.insert(0, src)
import numpy as np
from retroimpact.emulate import emulate, score_printers, detect_printer, split_segments, mask_graphics
from retroimpact.analyze import classify
from retroimpact.capture_fix import undouble, unfinished_graphics, is_probe
from retroimpact.model import EmuOptions
from retroimpact import samples
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import golden_mock  # noqa: F401  (installs the rasteriser stand-in)

ESC = b"\x1b"
inp = os.path.join(repo, "tests/golden/inputs")
os.makedirs(inp, exist_ok=True)


def fuzz_escp(seed: int) -> bytes:
    r = random.Random(seed)
    o = bytearray()
    words = [b"The quick brown fox", b"JUMPS over", b"0123456789", b"Hello, World!", b"{[|]}~ #$%&@", b"gjpqy Wmw ill"]
    for _ in range(r.randint(20, 80)):
        k = r.random()
        if k < 0.35:
            o += r.choice(words) + r.choice([b"", b"\r", b"\r\n", b"\n", b" "])
        elif k < 0.75:
            c = r.choice(b"@0123AJjCNOlQDBb/$\\ EFGH45-_STWwMPgpxk!Rtr67I:Uasimeqfuyzdhlcv=<>#89&.()[?")
            o += ESC + bytes([c]) + bytes(r.randrange(0, 40) for _ in range(r.randint(0, 4)))
        elif k < 0.85:
            m = r.choice([0, 1, 2, 3, 4, 5, 6, 7, 32, 33, 38, 39, 40, 71, 72, 73])
            bpc = {32: 3, 33: 3, 38: 3, 39: 3, 40: 3, 71: 6, 72: 6, 73: 6}.get(m, 1)
            n = r.randint(1, 60)
            o += ESC + b"*" + bytes([m, n & 255, n >> 8]) + bytes(r.randrange(256) for _ in range(n * bpc))
        elif k < 0.9:
            c = r.choice(b"KLYZ")
            n = r.randint(1, 60)
            o += ESC + bytes([c]) + bytes([n & 255, n >> 8]) + bytes(r.randrange(256) for _ in range(n))
        else:
            o += bytes([r.choice([8, 9, 10, 11, 12, 13, 14, 15, 18, 20, 7, 24, 127, 1, 2, 21, 28])])
    return bytes(o)



def fuzz_iw(seed: int) -> bytes:
    r = random.Random(seed)
    o = bytearray()
    words = [b"The quick brown fox", b"JUMPS over", b"0123456789", b"Hello, World!", b"caf\x8e \xa5 \xc7\xd0", b"gjpqy Wmw ill"]
    def dig(n, w): return b"%0*d" % (w, n)
    for _ in range(r.randint(20, 70)):
        k = r.random()
        if k < 0.3:
            o += r.choice(words) + r.choice([b"", b"\r", b"\r\n", b"\n", b" ", b"\x0c"])
        elif k < 0.62:
            c = r.choice([b"n", b"N", b"E", b"e", b"q", b"Q", b"p", b"P", b"A", b"B", b"f", b"r", b"c", b"!", b'"', b"X", b"Y", b"x", b"y", b"z", b"w", b"W", b"s", b"<", b">", b"v", b"Z", b"@"])
            o += b"\x1b" + c + (bytes([r.randrange(256)]) if c == b"s" else b"")
        elif k < 0.7:
            o += b"\x1bT" + dig(r.randint(0, 40), 2)
        elif k < 0.74:
            o += b"\x1bL" + dig(r.randint(0, 40), 3)
        elif k < 0.77:
            o += b"\x1bH" + dig(r.choice([0, 66, 88, 792, 1584, 100]), 4)
        elif k < 0.8:
            o += b"\x1bF" + dig(r.randint(0, 700), 4)
        elif k < 0.83:
            o += b"\x1bR" + dig(r.randint(1, 12), 3) + bytes([r.choice(b"-=*#")])
        elif k < 0.85:
            o += b"\x1bV" + dig(r.randint(1, 40), 4) + bytes([r.randrange(256)])
        elif k < 0.88:
            o += b"\x1b" + r.choice([b"a0", b"a1", b"a2"]) 
        elif k < 0.9:
            o += b"\x1bK" + bytes([48 + r.randint(0, 4)]) + r.choice([b"\r", b"\x1b"])
        elif k < 0.97:
            kind = r.choice([b"G", b"S", b"g"])
            cnt = r.randint(1, 120)
            nb = cnt * (8 if kind == b"g" else 1)
            body = bytes(r.randrange(256) for _ in range(nb))
            if r.random() < 0.25:  # lost data: next commands show up early
                cut = r.randint(0, nb - 1) if nb > 1 else 0
                body = body[:cut] + r.choice([b"\r\x1bK1", b"\r\n\r\x1b", b"\x1b>\x1bP\x1bG0010", b"\x1bG0004"])
            o += b"\x1b" + kind + dig(cnt, 3 if kind == b"g" else 4) + body + b"\r"
        else:
            o += bytes([r.choice([8, 9, 12, 13, 10, 7, 0x1b])]) + r.choice([b"", b"5", b"12Z", b"1A"])
    return bytes(o)


def fuzz_mps(seed: int, dense: bool = False) -> bytes:
    r = random.Random(seed)
    o = bytearray()
    for _ in range(r.randint(20, 70)):
        k = r.random()
        if k < 0.35:
            o += bytes(r.choice([0x41, 0x42, 0x61, 0x62, 0x20, 0x31, 0x2e, 0xc1, 0xc5, 0xdb, 0xa1, 0xb5, 0x5c, 0x5f, 0x7d]) for _ in range(r.randint(1, 30)))
            o += r.choice([b"\r", b"\r\n", b"\n", b""])
        elif k < 0.6:
            n = r.randint(1, 60) if not dense else r.randint(500, 560)
            o += b"\x08" + bytes(0x80 | r.randrange(128) for _ in range(n)) + r.choice([b"\r", b"\r\n", b"\n"])
        elif k < 0.67:
            o += bytes([26, r.randint(0, 255), 0x80 | r.randrange(128)])
        elif k < 0.8:
            o += bytes([r.choice([14, 15, 17, 145, 18, 146, 12, 13, 10, 0])])
        elif k < 0.85:
            o += b"\x10%02d" % r.randint(0, 79)
        elif k < 0.9:
            o += b"\x1b\x10" + bytes([r.randint(0, 1), r.randint(0, 255)])
        elif k < 0.93:
            o += b"\x1b-" + bytes([r.randint(0, 1)])
        else:
            o += b"\x1b" + bytes([r.randrange(256)])
    return bytes(o)


def okimate_chained(seed: int) -> bytes:
    r = random.Random(seed)
    o = bytearray(b"\x1b@")
    for band in range(r.randint(3, 8)):
        base = [r.randrange(256) if r.random() < 0.5 else 0 for _ in range(r.randint(20, 90))]
        for ps in range(3):
            body = bytes((b if r.random() < 0.9 else r.randrange(256)) for b in base)
            kind = r.choice([b"K", b"L", b"Y", b"Z"])
            o += b"\x1b" + kind + bytes([len(body) & 255, len(body) >> 8]) + body + b"\r"
        if r.random() < 0.5:
            o += b"\x1bA" + bytes([r.randint(8, 24)]) + b"\x1b2"
    if r.random() < 0.5:
        o += b"Hello Okimate\r\n"
    return bytes(o)


def okimate_geos(seed: int, black: bool = False) -> bytes:
    r = random.Random(seed)
    o = bytearray(b"\x1c\x1bP" if r.random() < 0.5 else b"")
    for band in range(r.randint(7, 12)):
        passes = 1 if black else 3
        if not black:
            o += b"\x1b\x19"
        for ps in range(passes):
            cols = bytearray()
            for _ in range(r.randint(8, 64) * 1):
                cols += bytes([r.randrange(256) if r.random() < 0.5 else 0, r.randrange(256) if r.random() < 0.5 else 0, 0])
            body = bytes(cols).replace(b"\x03", b"\x03\x03")
            o += b"\x03" + body + b"\x03\x02\x8d"
        o += b"\x1b\n\x10"
    return bytes(o)


def mixed(seed: int) -> bytes:
    r = random.Random(seed)
    parts = []
    gens = [lambda s: fuzz_escp(s), lambda s: fuzz_iw(s), lambda s: fuzz_mps(s)]
    last = -1
    for k in range(r.randint(2, 4)):
        g = r.choice([x for i, x in enumerate(gens) if i != last])
        last = gens.index(g)
        parts.append(g(r.randrange(10 ** 6)) * r.randint(1, 3))
    return b"".join(parts)


cases = []
def add(name, data, keys, **optkw):
    open(os.path.join(inp, name + ".bin"), "wb").write(data)
    cases.append((name, keys, optkw))

ALL = ["epson_fx", "epson_lq", "ibm", "star"]
add("sample_epson", samples.epson_text(), ALL + ["auto"])
add("sample_iw", samples.imagewriter(), ["imagewriter", "auto"])
add("sample_mps", samples.commodore_mps(), ["mps", "auto"])
for s in range(40):
    add(f"fuzz_escp_{s:02d}", fuzz_escp(s), ALL + (["auto"] if s % 4 == 0 else []))
for s in range(30):
    add(f"fuzz_iw_{s:02d}", fuzz_iw(s), ["imagewriter", "auto"] + (["epson_fx"] if s % 5 == 0 else []))
for s in range(30):
    add(f"fuzz_mps_{s:02d}", fuzz_mps(s), ["mps", "auto"])
for s in range(6):
    add(f"fuzz_mps_dense_{s:02d}", fuzz_mps(s, True), ["mps", "auto"])
add("fuzz_mps_fill", fuzz_mps(7), ["mps"], mps_gap="fill")
add("fuzz_mps_fill2", fuzz_mps(8), ["mps"], mps_gap="fill", cbm_charset="lower")
add("fuzz_mps_close", fuzz_mps(9), ["mps"], mps_gap="close", cbm_charset="ascii")
add("fuzz_mps_fill3", b"\x08" + b"".join(bytes([0x80 | (7 * (i % 3))] * 40) + b"\r" for i in range(6)), ["mps"], mps_gap="fill")
for s in range(10):
    add(f"okimate_chained_{s:02d}", okimate_chained(s), ["okimate", "auto"])
for s in range(6):
    add(f"okimate_geos_{s:02d}", okimate_geos(s, s % 3 == 2), ["okimate", "auto"])
for s in range(14):
    add(f"mixed_{s:02d}", mixed(s), ["auto"])
doubled = bytes(b for b in samples.epson_text() for _ in range(2))
add("doubled_epson", doubled, ["auto", "epson_fx"])
add("doubled_stray", b"\x55" + doubled, ["auto"])
add("fuzz_opts_a", fuzz_escp(101), ["epson_fx"], esc_a_immediate=False, codepage="cp850", blank_lines="collapse")
add("fuzz_opts_b", fuzz_escp(102), ["epson_lq"], honor_page_length=False, blank_lines="remove", force_nlq=True)
add("fuzz_opts_c", fuzz_escp(103), ["ibm"], codepage="latin1", auto_wrap=False, page_length_in=14.0)
add("fuzz_opts_d", fuzz_iw(104), ["imagewriter"], blank_lines="collapse", cr_adds_lf=True)
add("fuzz_opts_e", fuzz_escp(105), ["epson_fx"], wide_carriage=True, lf_adds_cr=False, cr_adds_lf=False)
for fn in sorted(os.listdir(os.path.join(src, "tests", "data"))):
    if fn.endswith(".prn"):
        d = open(os.path.join(src, "tests", "data", fn), "rb").read()
        add("data_" + fn[:-4], d, ["auto"])


def stats(doc):
    pages = []
    for p in doc.pages:
        d = dict(n=p.n_dots, w=p.width_in, h=p.height_in, unknown=p.unknown, pk=p.profile_key,
                 sx=float(p.x.astype(np.float64).sum()), sy=float(p.y.astype(np.float64).sum()),
                 xb=int(p.x.view(np.uint32).astype(np.uint64).sum()), yb=int(p.y.view(np.uint32).astype(np.uint64).sum()),
                 spin=int(p.pin.astype(np.int64).sum()), spass=int(p.pass_no.astype(np.int64).sum()),
                 sdir=int(p.dirn.astype(np.int64).sum()),
                 scol=int(p.color.astype(np.int64).sum()) if p.color is not None else -1)
        pages.append(d)
    return dict(pages=pages, n_unknown=doc.n_unknown, carriage=doc.carriage_in, warnings=len(doc.warnings),
                fixes=len(doc.fixes), notes=len(doc.notes), detected=doc.detected, n_bytes=doc.n_bytes)


out, skipped, analysis = {}, [], {}
for name, keys, optkw in cases:
    data = open(os.path.join(inp, name + ".bin"), "rb").read()
    for k in keys:
        try:
            doc = emulate(data, EmuOptions(printer=k, **optkw))
        except Exception as e:  # a bug in the original on hostile input: not something to port
            skipped.append((name, k, repr(e)))
            continue
        out[f"{name}|{k}"] = dict(opts=optkw, **stats(doc))
        analysis[f"{name}|{k}"] = [(i.kind, i.reason) for i in classify(doc)]

with open(os.path.join(repo, "tests/golden/emulate.tsv"), "w") as f:
    for key, e in sorted(out.items()):
        o = ";".join(f"{k}={v}" for k, v in sorted(e["opts"].items()))
        f.write(f"{key}\tDOC\t{len(e['pages'])}\t{e['n_unknown']}\t{e['carriage']}\t{o}\t{e['detected']}\t{e['warnings']}\t{e['fixes']}\t{e['notes']}\t{e['n_bytes']}\n")
        for i, p in enumerate(e["pages"]):
            f.write(f"{key}\t{i}\t{p['n']}\t{p['w']!r}\t{p['h']!r}\t{p['unknown']}\t{p['sx']!r}\t{p['sy']!r}\t{p['xb']}\t{p['yb']}\t{p['spin']}\t{p['spass']}\t{p['sdir']}\t{p['scol']}\t{p['pk']}\n")

with open(os.path.join(repo, "tests/golden/analyze.tsv"), "w") as f:
    for key, v in sorted(analysis.items()):
        for i, (kind, reason) in enumerate(v):
            f.write(f"{key}\t{i}\t{kind}\t{reason}\n")

# ---- detection / splitting / repair, independent of rendering
with open(os.path.join(repo, "tests/golden/detect.tsv"), "w") as f:
    for name, keys, optkw in cases:
        data = open(os.path.join(inp, name + ".bin"), "rb").read()
        sc = score_printers(data)
        segs = split_segments(data)
        und, note = undouble(data)
        f.write("\t".join([
            name, detect_printer(data),
            ",".join(f"{k}={v!r}" for k, v in sc.items()),
            ";".join(f"{a}-{b}-{k}" for a, b, k in segs),
            "1" if unfinished_graphics(data) else "0",
            "1" if is_probe(data) else "0",
            str(len(und)), str(sum(mask_graphics(data))), note,
        ]) + "\n")
print(len(out), "golden entries;", len(skipped), "skipped:", skipped[:5])
