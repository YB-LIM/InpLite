#!/usr/bin/env python3
"""Turn the full extracted schema into the compact JSON embedded in the app."""
import json, re, sys
src, dst = sys.argv[1], sys.argv[2]
s = json.load(open(src))
def first_sentences(t, n=260):
    t = t.strip()
    if len(t) <= n: return t
    cut = t[:n]
    i = cut.rfind('. ')
    return (cut[:i+1] if i > 80 else cut.rstrip() + '…')
out = []
for k in s['keywords']:
    params = {}
    for p, v in k['params'].items():
        e = {}
        if v.get('values'): e['v'] = v['values']
        if v.get('flag'): e['f'] = 1
        d = first_sentences(v.get('doc', ''))
        if d: e['d'] = d
        params[p] = e
    lv = []
    for l in k['level']:
        if l.startswith('This option is not supported'):
            lv.append('NoAssembly')
        else:
            lv.append(l)
    out.append(dict(n=k['name'], s=k['summary'], pr=k['products'], t=k['type'], l=lv, pg=k['page'],
                    r=k['required'], g=k['groups'], p=params, u=k['usecases']))
json.dump(dict(src=s['source'], k=out), open(dst, 'w'), separators=(',', ':'), ensure_ascii=True)
print(len(out), 'keywords ->', dst)
