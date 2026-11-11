#!/usr/bin/env python3
"""Upgrade test: data written by an OLD release must be readable, intact and consistent when opened by the NEW build.

    python3 tools/upgrade-test.py OLD_BINARY NEW_BINARY WORKDIR

The old binary is a published release (CI downloads the latest one); the new one is built from the commit under test. A fixture produced by
the current code proves nothing about upgrades, so the data here is written by the old server over HTTP: records with files, duplicate and
near-duplicate files (shared chunks), an encrypted collection, a multi-chunk file, a deleted record, and a restart in between.

What it checks, in order:
  1. the old server accepts the data and `check` passes on it;
  2. the new binary opens the directory (upgrading it in place if the layout changed), `check` passes, and every record and every file
     comes back byte for byte (SHA-256) through the new server;
  3. the new server can write to it (a new record, a deleted old one), and `check` still passes after a clean stop.
"""
import hashlib, json, os, random, re, signal, subprocess, sys, time, urllib.error, urllib.request

OLD, NEW, WORK = sys.argv[1], sys.argv[2], sys.argv[3]
DATA = os.path.join(WORK, "data")
PORT = 8470 + random.Random(os.getpid()).randrange(0, 200)
BASE = f"http://127.0.0.1:{PORT}/api/v1"
rnd = random.Random(7)


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def call(method, path, key, body=None, headers=None):
    h = {"authorization": f"Bearer {key}", **(headers or {})}
    req = urllib.request.Request(BASE + path, data=body, method=method, headers=h)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def js(method, path, key, obj):
    s, b = call(method, path, key, json.dumps(obj).encode(), {"content-type": "application/json"})
    return s, (json.loads(b) if b else None)


def upload(col, key, fields, name, data):
    b = "----up" + format(rnd.getrandbits(48), "x")
    parts = [f'--{b}\r\nContent-Disposition: form-data; name="data"\r\nContent-Type: application/json\r\n\r\n{json.dumps(fields)}\r\n'.encode(),
             f'--{b}\r\nContent-Disposition: form-data; name="doc"; filename="{name}"\r\nContent-Type: application/octet-stream\r\n\r\n'.encode(),
             data, f"\r\n--{b}--\r\n".encode()]
    s, body = call("POST", f"/collections/{col}/records", key, b"".join(parts), {"content-type": f"multipart/form-data; boundary={b}"})
    assert s == 201, (col, s, body[:200])
    return json.loads(body)["id"]


def blob(seed, size, tweak=None):
    r = random.Random(seed)
    b = bytearray(r.randbytes(size))
    if tweak:
        b[size // 2: size // 2 + 40] = bytes((x + tweak) % 256 for x in b[size // 2: size // 2 + 40])
    return bytes(b)


class Server:
    def __init__(self, binary):
        self.p = subprocess.Popen([binary, "serve", "--data", DATA, "--addr", f"127.0.0.1:{PORT}"], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        for _ in range(100):
            try:
                urllib.request.urlopen(f"{BASE}/healthz", timeout=1)
                return
            except Exception:
                if self.p.poll() is not None:
                    raise SystemExit("the server exited at start:\n" + self.p.stderr.read().decode(errors="replace")[-2000:])
                time.sleep(0.2)
        raise SystemExit("the server did not come up")

    def stop(self):
        self.p.send_signal(signal.SIGTERM)  # a clean stop, as a service manager would
        try:
            self.p.wait(30)
        except subprocess.TimeoutExpired:
            self.p.kill()
            raise SystemExit("the server did not stop within 30 s of SIGTERM")


def check(binary, label):
    r = run([binary, "check", "--data", DATA])
    if r.returncode != 0:
        raise SystemExit(f"{label}: `check` failed:\n{r.stdout}\n{r.stderr}")
    print(f"  {label}: check passed")


def sha(b):
    return hashlib.sha256(b).hexdigest()


def main():
    os.makedirs(WORK, exist_ok=True)
    print("old release:", run([OLD, "--version"]).stdout.strip(), "| new build:", run([NEW, "--version"]).stdout.strip())
    out = run([OLD, "init", DATA])
    m = re.search(r"pdb_live_[A-Za-z0-9_\-]+", out.stdout + out.stderr)
    assert m, "no administrator key printed by `init`"
    key = m.group(0)

    # --- 1. write with the old release
    srv = Server(OLD)
    s, d = js("POST", "/collections", key, {"name": "docs", "fields": [{"name": "title", "kind": "text"}, {"name": "doc", "kind": "file"}]})
    assert s == 201, (s, d)
    s, d = js("POST", "/collections", key, {"name": "vault", "encrypt": True, "fields": [{"name": "title", "kind": "text"}, {"name": "doc", "kind": "file"}]})
    assert s == 201, (s, d)
    expected = {}  # (collection, id) -> (title, sha256 of file)
    files = [
        ("docs", "small", blob(1, 5_000)),
        ("docs", "dup-a", blob(2, 400_000)),
        ("docs", "dup-b", blob(2, 400_000)),            # identical bytes: one stored file
        ("docs", "near", blob(2, 400_000, tweak=3)),    # shares most chunks with dup-a
        ("docs", "big", blob(3, 6_000_000)),            # several chunks
        ("vault", "secret-1", blob(4, 300_000)),
        ("vault", "secret-2", blob(4, 300_000)),        # identical, encrypted
        ("vault", "secret-3", blob(5, 80_000)),
    ]
    for col, title, data in files:
        expected[(col, upload(col, key, {"title": title}, title + ".bin", data))] = (title, sha(data))
    victim = next(k for k, v in expected.items() if v[0] == "dup-b")
    s, _ = call("DELETE", f"/collections/{victim[0]}/records/{victim[1]}", key)
    assert s in (200, 204), s
    del expected[victim]
    # objects (0.4 and later): a versioned bucket with several versions and a delete marker. An older release has no such routes: skipped.
    objects = {}
    s, _ = js("POST", "/buckets", key, {"name": "archive", "versioning": True})
    if s == 201:
        v1, v2, other = blob(21, 700_000), blob(21, 700_000, tweak=5), blob(22, 90_000)
        for k, data in [("legal/msa.pdf", v1), ("legal/msa.pdf", v2), ("notes.txt", other)]:
            st, b = call("PUT", f"/buckets/archive/objects/{k}", key, data, {"content-type": "application/octet-stream"})
            assert st == 200, (k, st, b[:200])
        st, _ = call("PUT", "/buckets/archive/objects/gone.bin", key, blob(23, 5000), {"content-type": "application/octet-stream"})
        assert st == 200
        st, _ = call("DELETE", "/buckets/archive/objects/gone.bin", key)
        assert st == 204
        objects = {"legal/msa.pdf": sha(v2), "notes.txt": sha(other)}
        print("  old release: wrote versioned objects")
    srv.stop()
    check(OLD, "old release")

    # --- 2. read with the new build
    check(NEW, "new build, before serving")
    srv = Server(NEW)
    for (col, rid), (title, digest) in expected.items():
        s, b = call("GET", f"/collections/{col}/records/{rid}", key)
        assert s == 200, (col, rid, s)
        rec = json.loads(b)
        assert rec.get("title") == title, (title, rec)
        s, body = call("GET", f"/files/{col}/{rid}/doc", key)
        assert s == 200, ("download", title, s)
        assert sha(body) == digest, f"{title}: the file differs after the upgrade"
    s, b = call("GET", f"/collections/docs/records/{victim[1]}", key)
    assert s == 404, f"the record deleted before the upgrade is back (status {s})"
    for k, digest in objects.items():
        st, body = call("GET", f"/buckets/archive/objects/{k}", key)
        assert st == 200 and sha(body) == digest, f"object {k} differs after the upgrade (status {st})"
    if objects:
        st, body = call("GET", "/buckets/archive/versions?key=legal/msa.pdf", key)
        assert st == 200 and len(json.loads(body)["items"]) == 2, "the versions of an object changed in the upgrade"
        st, _ = call("GET", "/buckets/archive/objects/gone.bin", key)
        assert st == 404, "an object deleted before the upgrade is back"
        print(f"  new build: {len(objects)} objects and their versions read back identical")
    print(f"  new build: {len(expected)} records and files read back identical")

    # --- 3. write with the new build
    nid = upload("docs", key, {"title": "after"}, "after.bin", blob(9, 700_000))
    expected[("docs", nid)] = ("after", sha(blob(9, 700_000)))
    old_id = next(k for k, v in expected.items() if v[0] == "near")
    s, _ = call("DELETE", f"/collections/{old_id[0]}/records/{old_id[1]}", key)
    assert s in (200, 204), s
    del expected[old_id]
    time.sleep(1)
    srv.stop()
    check(NEW, "new build, after writing")
    print("upgrade test passed")


main()
