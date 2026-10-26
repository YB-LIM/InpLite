#!/usr/bin/env python3
"""Extract a compact keyword schema (JSON) from the Abaqus Keywords Guide PDF.

Usage: python3 extract_schema.py <Abaqus_KEYWORDS.pdf> <out.json>

The PDF layout is used for structure:
  * keyword titles  : Helvetica-Bold ~13pt, text starts with "*NAME: summary"
  * use-case titles : Helvetica-Bold ~13pt, no leading "*"
  * section headers : Helvetica-Bold ~10pt ("Required parameters:", "Data lines ...:")
  * parameter names : regular text, left indent smaller than their description
"""
import json, re, sys
import pymupdf as fitz

LIG = {"ﬁ": "fi", "ﬂ": "fl", "ﬀ": "ff", "ﬃ": "ffi", "ﬄ": "ffl",
       "’": "'", "‘": "'", "“": '"', "”": '"', "–": "-", "—": "-",
       " ": " "}

def norm(s):
    for k, v in LIG.items():
        s = s.replace(k, v)
    return re.sub(r"\s+", " ", s).strip()

PARAM_RE = re.compile(r"^[A-Z][A-Z0-9]*(?:[ _\-/][A-Z0-9]+)*$")
DATA_HDR = re.compile(r"^(Data line|Data lines|First|Second|Third|Fourth|Fifth|Sixth|Seventh|Eighth|Ninth|Tenth|Subsequent|Each|Repeat|Last|Line|Lines|If )", re.I)

def lines_of(doc, start_page):
    for pno in range(start_page, doc.page_count):
        d = doc[pno].get_text("dict")
        for b in d["blocks"]:
            for l in b.get("lines", []):
                spans = [s for s in l["spans"] if s["text"].strip()]
                if not spans:
                    continue
                size = spans[0]["size"]
                if size < 9:          # running header/footer, page number
                    continue
                txt = norm("".join(s["text"] for s in l["spans"]))
                bold = all("Bold" in s["font"] for s in spans)
                first_bold = "Bold" in spans[0]["font"]
                yield dict(x=l["bbox"][0], size=size, bold=bold, first_bold=first_bold, text=txt, page=pno + 1)

def main(pdf, out):
    doc = fitz.open(pdf)
    # body starts at the first keyword title after the TOC
    start = 0
    for pno in range(doc.page_count):
        if "*ACOUSTIC CONTRIBUTION: " in doc[pno].get_text():
            start = pno
            break
    kws = {}
    cur = None
    usecase = None
    mode = None          # None | 'refs' | 'required' | 'optional' | 'xreq' | 'xopt' | 'atleast' | 'data'
    group = None
    param = None
    pending_hdr = ""
    title_cont = False

    def new_usecase(title):
        nonlocal usecase, mode, group, param
        usecase = dict(title=title, required=[], groups=[], params=[])
        cur["usecases"].append(usecase)
        mode, group, param = None, None, None

    for ln in lines_of(doc, start):
        t = ln["text"]
        big = ln["bold"] and ln["size"] > 12
        if big:
            m = re.match(r"^\*([A-Z0-9][A-Z0-9 \-/_]*?):\s*(.*)$", t)
            if m:
                name = m.group(1).strip()
                cur = dict(name=name, summary=m.group(2), products=[], type="", level=[],
                           usecases=[], params={}, page=ln["page"])
                kws[name] = cur
                usecase = None
                new_usecase("")
                title_cont = not t.endswith(".")
                continue
            if cur is not None:
                if title_cont:
                    cur["summary"] += " " + t
                    title_cont = not t.endswith(".")
                    continue
                # a use-case title; the first one replaces the implicit (empty) use case
                if usecase is not None and usecase["title"] == "" and not usecase["params"] and not usecase["required"]:
                    usecase["title"] = t
                    mode, group, param = None, None, None
                else:
                    new_usecase(t)
            continue
        title_cont = False
        if cur is None:
            continue
        if not cur.get("_headdone"):
            if t.startswith("References:") or (ln["bold"] and "parameter" in t.lower()) or t.startswith("There are no"):
                cur["_headdone"] = True
            else:
                cur["_head"] = cur.get("_head", "") + " " + t
        META = ("Products:", "Type:", "Level:", "Abaqus/CAE:", "References:")
        if ln["bold"] or (ln["first_bold"] and t.startswith(META)):
            h = (pending_hdr + " " + t).strip() if pending_hdr else t
            if not h.endswith(":") and not h.startswith(("Products", "Type", "Level", "Abaqus/CAE", "References")) and len(h) < 300:
                pending_hdr = h
                continue
            pending_hdr = ""
            param = None
            if h.startswith("Products:"):
                p = h[len("Products:"):]
                cur["products"] = sorted(set(
                    {"Abaqus/Standard": "S", "Abaqus/Explicit": "E", "Abaqus/CFD": "CFD", "Abaqus/CAE": "CAE"}.get(w, w)
                    for w in p.split() if w.startswith("Abaqus/")))
                continue
            if h.startswith("Type:"):
                cur["type"] = h[5:].strip()
                continue
            if h.startswith("Level:"):
                cur["level"] = [w.strip() for w in h[6:].split(",") if w.strip()]
                continue
            if h.startswith("References"):
                mode = "refs"; continue
            if h.startswith("Abaqus/CAE"):
                mode = None; continue
            low = h.lower()
            if "parameter" in low and not DATA_HDR.match(h) and not low.startswith(("no parameters", "there are no", "there is no")):
                group = None
                if re.match(r"^required parameters:$", low):
                    mode = "required"
                elif low in ("required, mutually exclusive parameters:", "one of the following mutually exclusive parameters is required:"):
                    mode = "xreq"; group = dict(kind="one_of", params=[]); usecase["groups"].append(group)
                elif low.startswith("required, mutually exclusive") or low.startswith("one of the following mutually exclusive"):
                    mode = "xopt"; group = dict(kind="exclusive", params=[]); usecase["groups"].append(group)
                elif low == "at least one of the following parameters is required:":
                    mode = "atleast"; group = dict(kind="at_least_one", params=[]); usecase["groups"].append(group)
                elif low.startswith("optional, mutually exclusive"):
                    mode = "xopt"; group = dict(kind="exclusive", params=[]); usecase["groups"].append(group)
                else:
                    mode = "optional"
                continue
            mode = "data"
            continue
        # regular text
        if mode in ("required", "optional", "xreq", "xopt", "atleast"):
            if PARAM_RE.match(t) and len(t) <= 40 and ln["x"] < 125:
                pname = t
                param = cur["params"].setdefault(pname, dict(desc=""))
                usecase["params"].append(pname)
                if mode == "required":
                    usecase.setdefault("_reqdesc", {})[pname] = ""
                    usecase["required"].append(pname)
                if group is not None:
                    group["params"].append(pname)
                param["_cur"] = True
                cur["_lastp"] = pname
                continue
            if param is not None:
                pname = cur["_lastp"]
                param["desc"] = (param["desc"] + " " + t).strip()
                if mode == "required":
                    usecase["_reqdesc"][pname] = (usecase["_reqdesc"].get(pname, "") + " " + t).strip()
    # ---- post-process ----------------------------------------------------
    out_kws = []
    COND = re.compile(r"\b(only|if|when|unless|exclusive|applies|relevant|cannot|ignored|alternatively|must be used with|required for)\b|Abaqus/(Standard|Explicit|CFD)", re.I)
    for name, k in sorted(kws.items()):
        params = {}
        for pname, p in k["params"].items():
            d = p["desc"]
            vals = []
            # "Set P=A (default) to ...", "Set P=A, B, C, or D to ...", and inline "P=A" mentions
            for m in re.finditer(r"\bSet\s+" + re.escape(pname) + r"\s*=\s*(.+?)(?=\s+(?:to|if|for|when|in|which|so|that|with)\b|\.\s|\.$|;|:|\(|$)", d):
                for item in re.split(r",\s*(?:or\s+|and\s+)?|\s+or\s+|\s+and\s+", m.group(1)):
                    item = item.strip().rstrip(".")
                    if re.fullmatch(r"[A-Z0-9][A-Z0-9 _\-\.]*", item) and not re.fullmatch(r"[0-9.eE+\-]+", item):
                        vals.append(item)
            for m in re.finditer(r"\b" + re.escape(pname) + r"\s*=\s*([A-Z0-9][A-Z0-9_\-\.]*(?: [A-Z][A-Z0-9_\-]*)*)", d):
                v = re.split(r"\.\s", m.group(1))[0].rstrip(".")
                if v and not re.fullmatch(r"[0-9.eE+\-]+", v):
                    vals.append(v)
            setcount = len(re.findall(r"\bSet\s+" + re.escape(pname) + r"\s*=", d))
            vals = sorted(set(vals))
            entry = {}
            flag = ("Include this parameter" in d) and ("Set this parameter" not in d) and setcount == 0
            if flag:
                entry["flag"] = True
            if len(vals) >= 2 and setcount >= 2 and not re.search(r"Set this parameter equal to (?:the name|a |an |either)", d) and "user-defined" not in d.lower():
                entry["values"] = vals
            entry["doc"] = d[:400]
            params[pname] = entry
        # required: required in every use case that lists any parameters (unconditional wording)
        req_sets = []
        groups = []
        for uc in k["usecases"]:
            if not uc["params"] and not uc["required"]:
                continue
            strict = set()
            for pname in uc["required"]:
                desc = uc.get("_reqdesc", {}).get(pname, "")
                if not COND.search(desc):
                    strict.add(pname)
            # params named in "X and Y parameters are mutually exclusive" sentences are not strictly required
            req_sets.append(strict)
            for g in uc["groups"]:
                if g["params"]:
                    groups.append(dict(kind=g["kind"], params=g["params"], usecase=uc["title"]))
        required = sorted(set.intersection(*req_sets)) if req_sets else []
        # one_of / at_least_one groups are only enforced if the keyword has a single use case
        multi = len([u for u in k["usecases"] if u["params"] or u["required"]]) > 1
        gl = []
        for g in groups:
            kind = g["kind"]
            if kind in ("one_of", "at_least_one") and multi:
                kind = "exclusive" if kind == "one_of" else None
            if kind:
                g2 = dict(kind=kind, params=g["params"])
                if g2 not in gl:
                    gl.append(g2)
        # "The A and B parameters are mutually exclusive." sentences -> exclusive pairs
        for pname, p in k["params"].items():
            for m in re.finditer(r"The ([A-Z][A-Z0-9 _\-,]*?) parameters are mutually exclusive", p["desc"]):
                names = [n.strip() for n in re.split(r",\s*(?:and\s+)?|\s+and\s+", m.group(1)) if n.strip()]
                names = [n for n in names if n in k["params"]]
                if len(names) >= 2:
                    g = dict(kind="exclusive", params=sorted(set(names)))
                    if g not in gl:
                        gl.append(g)
        head = k.get("_head", "")
        mp = re.search(r"Products:\s*(.*?)(?=Type:|Level:|Abaqus/CAE:|$)", head)
        if mp:
            k["products"] = sorted(set({"Abaqus/Standard": "S", "Abaqus/Explicit": "E", "Abaqus/CFD": "CFD", "Abaqus/CAE": "CAE"}.get(w, w)
                                       for w in mp.group(1).split() if w.startswith("Abaqus/")))
        mt = re.search(r"Type:\s*(.*?)(?=Level:|Abaqus/CAE:|Products:|$)", head)
        if mt:
            k["type"] = mt.group(1).strip()
        ml = re.search(r"Level:\s*(.*?)(?=Abaqus/CAE:|Products:|Type:|$)", head)
        if ml:
            k["level"] = [w.strip() for w in ml.group(1).split(",") if w.strip()]
        typ = k["type"].lower()
        kt = "model" if typ.startswith("model data") else "history" if typ.startswith("history") else "both" if "model or history" in typ or "model and history" in typ else (typ or "?")
        out_kws.append(dict(
            name=name, summary=k["summary"], products=[p for p in k["products"] if p != "CAE"],
            cae=("CAE" in k["products"]), type=kt, level=k["level"], page=k["page"],
            usecases=[u["title"] for u in k["usecases"] if u["title"]],
            required=required, groups=gl, params=params))
    json.dump(dict(source="Abaqus 2025 Keywords Guide", keywords=out_kws), open(out, "w"), indent=0)
    print(f"{len(out_kws)} keywords, {sum(len(k['params']) for k in out_kws)} params, "
          f"{sum(1 for k in out_kws for p in k['params'].values() if 'values' in p)} enum params, "
          f"{sum(len(k['required']) for k in out_kws)} required")

if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
