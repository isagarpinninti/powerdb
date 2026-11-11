#!/usr/bin/env python3
"""Fills a running PowerDB server with a small, realistic data set (fictional companies, invoices with PDFs, product images,
users, an API key). Used to take the screenshots in the README; handy for trying things out.

    powerdb init demo-data            # prints an admin key
    powerdb serve --data demo-data &
    python3 tools/demo-data.py http://127.0.0.1:8090 pdb_live_...
"""
import json, random, struct, sys, urllib.parse, urllib.request, zlib

BASE, KEY = sys.argv[1].rstrip("/"), sys.argv[2]
rnd = random.Random(11)


def call(method, path, body=None, headers=None):
    h = {"authorization": f"Bearer {KEY}"}
    h.update(headers or {})
    req = urllib.request.Request(BASE + "/api/v1" + path, data=body, method=method, headers=h)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def js(method, path, obj):
    return call(method, path, json.dumps(obj).encode(), {"content-type": "application/json"})


def upload(col, fields, name, mime, data, field):
    b = "----demo" + format(rnd.getrandbits(48), "x")
    parts = [f'--{b}\r\nContent-Disposition: form-data; name="data"\r\nContent-Type: application/json\r\n\r\n{json.dumps(fields)}\r\n'.encode(),
             f'--{b}\r\nContent-Disposition: form-data; name="{field}"; filename="{name}"\r\nContent-Type: {mime}\r\n\r\n'.encode(), data, f"\r\n--{b}--\r\n".encode()]
    return call("POST", f"/collections/{col}/records", b"".join(parts), {"content-type": f"multipart/form-data; boundary={b}"})


def png(w, h, c1, c2):
    """A plain gradient tile with a lighter rounded block: enough to show up as a thumbnail."""
    rows = []
    for y in range(h):
        row = bytearray([0])
        for x in range(w):
            t = (x / w + y / h) / 2
            px = [int(c1[i] + (c2[i] - c1[i]) * t) for i in range(3)]
            if w * 0.28 < x < w * 0.72 and h * 0.3 < y < h * 0.7:
                px = [min(255, v + 55) for v in px]
            row += bytes(px)
        rows.append(bytes(row))
    raw = zlib.compress(b"".join(rows), 9)
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", raw) + chunk(b"IEND", b"")


def pdf(title, lines):
    stream = "BT /F1 18 Tf 60 740 Td (%s) Tj /F1 11 Tf 0 -34 Td %s ET" % (title, " ".join(f"({l}) Tj 0 -18 Td" for l in lines))
    objs = ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
            f"<< /Length {len(stream)} >>\nstream\n{stream}\nendstream", "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    out, offs = b"%PDF-1.4\n", []
    for i, o in enumerate(objs, 1):
        offs.append(len(out))
        out += f"{i} 0 obj\n{o}\nendobj\n".encode()
    x = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode() + b"".join(f"{o:010d} 00000 n \n".encode() for o in offs)
    return out + f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n".encode()


def collection(name, fields, **kw):
    s, d = js("POST", "/collections", {"name": name, "fields": fields, **kw})
    assert s == 201, (name, s, d)


collection("customers", [{"name": "name", "kind": "text", "required": True}, {"name": "email", "kind": "text"}, {"name": "plan", "kind": "text"},
                         {"name": "seats", "kind": "number"}, {"name": "active", "kind": "bool"}],
           rules={"list": '@request.auth.id != ""', "view": '@request.auth.id != ""'})
collection("products", [{"name": "name", "kind": "text", "required": True}, {"name": "sku", "kind": "text"}, {"name": "price", "kind": "number"},
                        {"name": "image", "kind": "file", "max": 2000000, "mime": ["image/*"]}],
           rules={"list": "", "view": ""})
collection("invoices", [{"name": "number", "kind": "text", "required": True}, {"name": "customer", "kind": "text"}, {"name": "amount", "kind": "number"},
                        {"name": "status", "kind": "text"}, {"name": "issued", "kind": "text"}, {"name": "pdf", "kind": "file", "max": 5000000, "mime": ["application/pdf"]}],
           rules={"list": '@request.auth.id != ""', "view": '@request.auth.id != ""', "create": '@request.auth.id != ""'})
collection("contracts", [{"name": "title", "kind": "text", "required": True}, {"name": "signed_by", "kind": "text"},
                         {"name": "file", "kind": "file", "max": 5000000, "mime": ["application/pdf"]}], encrypt=True)

companies = [("Northwind Traders", "ap@northwind.example", "Team", 24), ("Halcyon Labs", "billing@halcyon.example", "Business", 80), ("Brightwater Clinic", "office@brightwater.example", "Team", 12),
             ("Kestrel Logistics", "finance@kestrel.example", "Business", 140), ("Orchard & Vine", "hello@orchardvine.example", "Starter", 4), ("Tallis Architects", "accounts@tallis.example", "Team", 31),
             ("Redfern Press", "pay@redfern.example", "Starter", 6), ("Meridian Credit Union", "it@meridiancu.example", "Enterprise", 420), ("Alder & Finch", "ops@alderfinch.example", "Team", 18),
             ("Fenwick Foods", "ar@fenwickfoods.example", "Business", 65), ("Quarry Hill School", "admin@quarryhill.example", "Starter", 9), ("Solent Marine", "billing@solentmarine.example", "Business", 52)]
for n, e, p, s in companies:
    assert js("POST", "/collections/customers/records", {"name": n, "email": e, "plan": p, "seats": s, "active": rnd.random() > 0.15})[0] == 201

shapes = [("Desk lamp", "LMP-100", 49, ((38, 70, 83), (42, 157, 143))), ("Standing desk", "DSK-220", 449, ((69, 123, 157), (168, 218, 220))), ("Monitor arm", "ARM-310", 89, ((106, 76, 147), (200, 160, 220))),
          ("Ergonomic chair", "CHR-440", 329, ((166, 61, 64), (244, 162, 97))), ("Cable tray", "CBL-050", 19, ((52, 78, 65), (163, 177, 138))), ("Whiteboard", "WBD-600", 129, ((33, 37, 41), (108, 117, 125)))]
for name, sku, price, (c1, c2) in shapes:
    assert upload("products", {"name": name, "sku": sku, "price": price}, f"{sku}.png", "image/png", png(240, 160, c1, c2), "image")[0] == 201

status = ["paid", "paid", "paid", "open", "overdue"]
for i in range(1042, 1042 + 36):
    c = rnd.choice(companies)[0]
    amt = rnd.choice([420, 980, 1250, 2400, 3100, 640, 7800, 215])
    st = rnd.choice(status)
    date = f"2026-{rnd.randint(8, 10):02d}-{rnd.randint(1, 28):02d}"
    doc = pdf(f"Invoice INV-{i}", [f"Billed to: {c}", f"Amount: USD {amt}.00", f"Status: {st}", f"Issued: {date}"])
    assert upload("invoices", {"number": f"INV-{i}", "customer": c, "amount": amt, "status": st, "issued": date}, f"INV-{i}.pdf", "application/pdf", doc, "pdf")[0] == 201

for t, who in [("Master services agreement", "Meridian Credit Union"), ("Data processing addendum", "Halcyon Labs"), ("Support terms, 2026", "Kestrel Logistics")]:
    assert upload("contracts", {"title": t, "signed_by": who}, "contract.pdf", "application/pdf", pdf(t, [f"Counterparty: {who}", "Signed electronically."]), "file")[0] == 201

# buckets: files with no record behind them
assert js("POST", "/buckets", {"name": "documents", "versioning": True})[0] == 201
assert js("POST", "/buckets", {"name": "exports"})[0] == 201


def put_object(bucket, key, mime, data, meta=None):
    h = {"content-type": mime, **{f"x-powerdb-meta-{k}": v for k, v in (meta or {}).items()}}
    s, _ = call("PUT", f"/buckets/{bucket}/objects/{urllib.parse.quote(key)}", data, h)
    assert s == 200, (bucket, key, s)


put_object("documents", "legal/msa-meridian.pdf", "application/pdf", pdf("Master services agreement", ["Counterparty: Meridian Credit Union", "Draft 1"]), {"customer": "Meridian Credit Union"})
put_object("documents", "legal/msa-meridian.pdf", "application/pdf", pdf("Master services agreement", ["Counterparty: Meridian Credit Union", "Draft 2, with the liability change"]), {"customer": "Meridian Credit Union"})
put_object("documents", "legal/msa-meridian.pdf", "application/pdf", pdf("Master services agreement", ["Counterparty: Meridian Credit Union", "Signed electronically."]), {"customer": "Meridian Credit Union", "status": "signed"})
put_object("documents", "legal/dpa-halcyon.pdf", "application/pdf", pdf("Data processing addendum", ["Counterparty: Halcyon Labs"]), {"customer": "Halcyon Labs"})
put_object("documents", "brand/logo-light.png", "image/png", png(240, 160, (38, 70, 83), (42, 157, 143)))
put_object("documents", "brand/logo-dark.png", "image/png", png(240, 160, (33, 37, 41), (108, 117, 125)))
put_object("documents", "brand/guidelines.pdf", "application/pdf", pdf("Brand guidelines", ["Colours, type and logo use."]))
put_object("documents", "onboarding/welcome-pack.pdf", "application/pdf", pdf("Welcome pack", ["Everything a new customer needs."]))
put_object("exports", "customers/2026-10.csv", "text/csv", b"name,email,plan,seats\n" + b"".join(f"{n},{e},{p},{s}\n".encode() for n, e, p, s in companies))
put_object("exports", "customers/2026-09.csv", "text/csv", b"name,email,plan,seats\n" + b"".join(f"{n},{e},{p},{s}\n".encode() for n, e, p, s in companies[:9]))

for e in ["maya@northwind.example", "dev@halcyon.example", "support@brightwater.example"]:
    assert js("POST", "/users", {"email": e, "password": "demo-password-123"})[0] == 201
s, k = js("POST", "/keys", {"name": "reporting service", "access": "read", "collections": ["invoices", "customers"]})
assert s == 201, k
print("demo data ready: 4 collections, 12 customers, 6 products, 36 invoices, 3 contracts (encrypted), 2 buckets with 10 objects, 3 users, 1 API key")
