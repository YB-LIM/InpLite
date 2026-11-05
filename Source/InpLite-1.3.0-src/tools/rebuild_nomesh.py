"""Rebuild a runnable-looking test model from an InpLite "export without mesh" file by
synthesising the omitted data lines (same line counts; node/element tables get unique labels).
Usage: python rebuild_nomesh.py in_nomesh.inp out.inp"""
import re, sys
NN = {'s4rs': 4, 's4r': 4, 's3rs': 3, 's3r': 3, 'c3d8r': 8, 'c3d8': 8, 'c3d4': 4, 'c3d6': 6, 'c3d10': 10,
      'c3d10m': 10, 'b31': 2, 'conn3d2': 2, 'mass': 1, 'rotaryi': 1, 't3d2': 2, 'spring1': 1, 'springa': 2}
src, dst = sys.argv[1], sys.argv[2]
om1 = re.compile(r'^\*\* \[(?:InpLite|Abapad\+\+)\] omitted \*(\S+) data: ([\d,]+) lines')
om2 = re.compile(r'^\*\* \[(?:InpLite|Abapad\+\+)\] ([\d,]+) data lines omitted')
nid = 0
eid = 0
last_kw = ''
with open(src, newline='') as f, open(dst, 'w', newline='') as w:
    for line in f:
        s = line.rstrip('\r\n')
        m1, m2 = om1.match(s), om2.match(s)
        if s.startswith('**') and not (m1 or m2) and s.startswith('** Exported by '):
            continue
        if m1 or m2:
            n = int((m1 or m2).group(2 if m1 else 1).replace(',', ''))
            kw = last_kw.lower().replace(' ', '')
            out = []
            if kw.startswith('*node'):
                for _ in range(n):
                    nid += 1
                    out.append(f"{nid:10d},{(nid % 1000) * 0.5:14.6f},{(nid // 1000 % 1000) * 0.5:14.6f},{(nid // 1000000) * 0.5:14.6f}")
            elif kw.startswith('*element'):
                t = re.search(r'type=([a-z0-9]+)', kw)
                k = NN.get(t.group(1) if t else '', 2)
                for _ in range(n):
                    eid += 1
                    a = eid % max(nid - 20, 1) + 1
                    out.append(f"{eid:10d}," + ",".join(f"{a + j:10d}" for j in range(k)))
            else:
                out = ["1, 2, 3"] * n
            w.write("\r\n".join(out) + "\r\n")
            continue
        if s.startswith('*') and not s.startswith('**'):
            last_kw = s
        w.write(line)
