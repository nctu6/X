#!/usr/bin/env python3
"""Minimal Google Drive v3 client for xfeed (X) backups (stdlib only, no rclone).

Credentials: JSON file (default $X_BACKUP_CONF/gdrive.json, i.e.
  ~/.config/x-backup/gdrive.json, mode 600)
  {"client_id": "...", "client_secret": "...", "refresh_token": "..."}
Scope is drive.file: only files/folders created by this OAuth client are visible.
The OAuth client may be shared with other tools; this one uses its own Drive root folder.

Usage:
  gdrive.py upload <local_dir>            # -> X-backup/<basename>/, md5-verified
  gdrive.py list                          # backups under X-backup/
  gdrive.py prune --days 90 [--dry-run]   # delete backup folders older than N days
  gdrive.py download <TS> <dest_dir>      # download X-backup/<TS>/ and md5-verify
  gdrive.py latest                        # print the newest complete <TS> (has SHA256SUMS)
  gdrive.py verify <TS> [--local-dir DIR] # exit 0 only if X-backup/<TS>/ holds every local
                                          # file with the same md5 and size, and SHA256SUMS
Env: X_GDRIVE_CREDS (creds path), X_BACKUP_CONF (default config dir),
     X_GDRIVE_ROOT (folder name, default X-backup), X_BACKUP_ROOT (verify's
     parent of <TS> when --local-dir is not given)
"""
import argparse, datetime as dt, hashlib, json, os, re, sys, time
import urllib.error, urllib.parse, urllib.request

CONF_DIR = os.environ.get("X_BACKUP_CONF") or os.path.join(
    os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config"), "x-backup")
CREDS = os.environ.get("X_GDRIVE_CREDS", os.path.join(CONF_DIR, "gdrive.json"))
ROOT_NAME = os.environ.get("X_GDRIVE_ROOT", "X-backup")
API = "https://www.googleapis.com/drive/v3"
UPLOAD = "https://www.googleapis.com/upload/drive/v3/files"
FOLDER = "application/vnd.google-apps.folder"
CHUNK = 8 * 1024 * 1024  # multiple of 256 KiB

_token = None


def log(msg):
    print(msg, flush=True)


def _request(method, url, data=None, headers=None, auth=True, raw=False, retries=5):
    headers = dict(headers or {})
    for attempt in range(retries + 1):
        if auth:
            headers["Authorization"] = "Bearer " + access_token()
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                body = r.read()
                if raw:
                    return r, body
                return json.loads(body) if body else {}
        except urllib.error.HTTPError as e:
            body = e.read()
            if raw and e.code == 308:  # resumable "Resume Incomplete"
                return e, body
            if e.code == 401 and auth and attempt < retries:
                global _token
                _token = None
                continue
            if e.code in (403, 429, 500, 502, 503, 504) and attempt < retries:
                if e.code != 403 or b"rateLimit" in body or b"userRateLimit" in body:
                    time.sleep(min(2 ** attempt, 30))
                    continue
            raise SystemExit(f"HTTP {e.code} {method} {url.split('?')[0]}: {body[:300].decode(errors='replace')}")
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            if attempt < retries:
                time.sleep(min(2 ** attempt, 30))
                continue
            raise SystemExit(f"network error {method} {url.split('?')[0]}: {e}")


def access_token():
    global _token
    if _token:
        return _token
    with open(CREDS) as f:
        c = json.load(f)
    data = urllib.parse.urlencode({
        "client_id": c["client_id"], "client_secret": c["client_secret"],
        "refresh_token": c["refresh_token"], "grant_type": "refresh_token",
    }).encode()
    res = _request("POST", "https://oauth2.googleapis.com/token", data=data,
                   headers={"Content-Type": "application/x-www-form-urlencoded"}, auth=False)
    _token = res["access_token"]
    return _token


def q_escape(s):
    return s.replace("\\", "\\\\").replace("'", "\\'")


def list_files(q, fields="id,name,mimeType,size,md5Checksum,createdTime"):
    out, page = [], None
    while True:
        params = {"q": q, "fields": f"nextPageToken,files({fields})", "pageSize": 1000,
                  "spaces": "drive", "orderBy": "createdTime"}
        if page:
            params["pageToken"] = page
        res = _request("GET", f"{API}/files?" + urllib.parse.urlencode(params))
        out += res.get("files", [])
        page = res.get("nextPageToken")
        if not page:
            return out


def create_folder(name, parent=None):
    meta = {"name": name, "mimeType": FOLDER}
    if parent:
        meta["parents"] = [parent]
    return _request("POST", f"{API}/files?fields=id,name", data=json.dumps(meta).encode(),
                    headers={"Content-Type": "application/json"})["id"]


def root_folder(create=False):
    found = list_files(f"name='{q_escape(ROOT_NAME)}' and mimeType='{FOLDER}' and 'root' in parents and trashed=false")
    if found:
        if len(found) > 1:
            log(f"warning: {len(found)} '{ROOT_NAME}' folders visible; using oldest {found[0]['id']}")
        return found[0]["id"]
    if not create:
        raise SystemExit(f"no '{ROOT_NAME}' folder visible to this client")
    fid = create_folder(ROOT_NAME)
    log(f"created root folder {ROOT_NAME} ({fid})")
    return fid


def children(parent, folders_only=False):
    q = f"'{parent}' in parents and trashed=false"
    if folders_only:
        q += f" and mimeType='{FOLDER}'"
    return list_files(q)


def md5_file(path):
    h = hashlib.md5()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def upload_file(path, parent):
    size = os.path.getsize(path)
    meta = json.dumps({"name": os.path.basename(path), "parents": [parent]}).encode()
    r, _ = _request("POST", f"{UPLOAD}?uploadType=resumable&fields=id,name,size,md5Checksum", data=meta,
                    headers={"Content-Type": "application/json; charset=UTF-8",
                             "X-Upload-Content-Type": "application/octet-stream",
                             "X-Upload-Content-Length": str(size)}, raw=True)
    session = r.headers["Location"]
    with open(path, "rb") as f:
        if size == 0:
            return _request("PUT", session, data=b"", headers={"Content-Length": "0"})
        offset = 0
        while offset < size:
            f.seek(offset)
            chunk = f.read(CHUNK)
            end = offset + len(chunk) - 1
            r, body = _request("PUT", session, data=chunk, raw=True, headers={
                "Content-Length": str(len(chunk)), "Content-Range": f"bytes {offset}-{end}/{size}"})
            if getattr(r, "status", getattr(r, "code", 0)) in (200, 201):
                return json.loads(body)
            rng = r.headers.get("Range")  # e.g. bytes=0-8388607
            offset = int(rng.split("-")[1]) + 1 if rng else 0
    raise SystemExit(f"upload of {path} did not complete")


def cmd_upload(a):
    src = os.path.abspath(a.local_dir)
    ts = os.path.basename(src.rstrip("/"))
    root = root_folder(create=True)
    if list_files(f"name='{q_escape(ts)}' and '{root}' in parents and trashed=false"):
        raise SystemExit(f"{ROOT_NAME}/{ts} already exists on Drive")
    folder = create_folder(ts, root)
    # SHA256SUMS goes last, so a folder that has it is a complete upload (see `latest`).
    names = sorted((n for n in os.listdir(src) if os.path.isfile(os.path.join(src, n))),
                   key=lambda n: (n == "SHA256SUMS", n))
    bad = 0
    for n in names:
        p = os.path.join(src, n)
        res = upload_file(p, folder)
        local = md5_file(p)
        ok = res.get("md5Checksum") == local and int(res.get("size", -1)) == os.path.getsize(p)
        bad += not ok
        log(f"{'ok ' if ok else 'BAD'} {n} {os.path.getsize(p)}B md5={local}")
    if bad:
        raise SystemExit(f"{bad} file(s) failed md5/size verification")
    log(f"uploaded {len(names)} files to {ROOT_NAME}/{ts} ({folder})")


def cmd_list(a):
    root = root_folder()
    for d in children(root, folders_only=True):
        files = children(d["id"])
        total = sum(int(f.get("size", 0)) for f in files)
        log(f"{d['name']}  created={d['createdTime']}  files={len(files)}  bytes={total}")


def cmd_prune(a):
    root = root_folder()
    cutoff = dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=a.days)
    old = [d for d in children(root, folders_only=True)
           if dt.datetime.fromisoformat(d["createdTime"].replace("Z", "+00:00")) < cutoff]
    log(f"prune: {len(old)} folder(s) older than {a.days} days (cutoff {cutoff:%F %T} UTC){' [dry-run]' if a.dry_run else ''}")
    for d in old:
        log(f"  {'would delete' if a.dry_run else 'delete'} {d['name']} created={d['createdTime']}")
        if not a.dry_run:
            _request("DELETE", f"{API}/files/{d['id']}")  # permanent, removes contents


def cmd_latest(a):
    root = root_folder()
    for d in sorted(children(root, folders_only=True), key=lambda d: d["name"], reverse=True):
        if not re.fullmatch(r"\d{8}_\d{6}", d["name"]):
            continue
        if list_files(f"name='SHA256SUMS' and '{d['id']}' in parents and trashed=false", fields="id"):
            print(d["name"])
            return
        print(f"skipping incomplete backup {d['name']} (no SHA256SUMS)", file=sys.stderr)
    raise SystemExit(f"no complete backup under {ROOT_NAME}/")


def cmd_download(a):
    root = root_folder()
    found = list_files(f"name='{q_escape(a.ts)}' and '{root}' in parents and mimeType='{FOLDER}' and trashed=false")
    if not found:
        raise SystemExit(f"{ROOT_NAME}/{a.ts} not found")
    os.makedirs(a.dest_dir, exist_ok=True)
    bad = 0
    for f in children(found[0]["id"]):
        dest = os.path.join(a.dest_dir, f["name"])
        _, body = _request("GET", f"{API}/files/{f['id']}?alt=media", raw=True)
        with open(dest, "wb") as out:
            out.write(body)
        ok = hashlib.md5(body).hexdigest() == f.get("md5Checksum")
        bad += not ok
        log(f"{'ok ' if ok else 'BAD'} {f['name']} {len(body)}B")
    if bad:
        raise SystemExit(f"{bad} file(s) failed md5 verification")


def _verify_one(local, folder):
    """Problems with one Drive folder against the local snapshot ([] = verified)."""
    remote = {}
    for f in children(folder["id"]):
        remote.setdefault(f["name"], []).append(f)
    problems = []
    for n, size, md5 in local:
        cands = remote.get(n, [])
        if not cands:
            problems.append(f"{n}: missing on Drive")
        elif not any(f.get("md5Checksum") == md5 and int(f.get("size", -1)) == size for f in cands):
            problems.append(f"{n}: md5/size differ on Drive")
    return problems


def cmd_verify(a):
    # Read-only. Any problem (or a Drive/network error) exits non-zero, so the
    # caller keeps the local copy.
    ts = os.path.basename(a.ts.rstrip("/"))
    if not re.fullmatch(r"\d{8}_\d{6}", ts):
        raise SystemExit(f"verify {ts}: not a <YYYYmmdd_HHMMSS> snapshot name")
    if a.local_dir:
        src = a.local_dir
    elif os.path.isdir(a.ts) and os.sep in a.ts:
        src = a.ts
    else:
        root = os.environ.get("X_BACKUP_ROOT")
        if not root:
            raise SystemExit(f"verify {ts}: pass --local-dir or set X_BACKUP_ROOT")
        src = os.path.join(root, ts)
    src = os.path.abspath(src)
    if not os.path.isdir(src):
        raise SystemExit(f"verify {ts}: local dir {src} not found")
    entries = sorted(os.listdir(src))
    others = [n for n in entries if not os.path.isfile(os.path.join(src, n)) or os.path.islink(os.path.join(src, n))]
    if others:  # upload only sends regular files; anything else would be lost
        raise SystemExit(f"verify {ts}: local dir has non-regular entries {others}")
    if "SHA256SUMS" not in entries:
        raise SystemExit(f"verify {ts}: no local SHA256SUMS")
    local = [(n, os.path.getsize(os.path.join(src, n)), md5_file(os.path.join(src, n))) for n in entries]
    root = root_folder()
    found = list_files(f"name='{q_escape(ts)}' and '{root}' in parents and mimeType='{FOLDER}' and trashed=false")
    if not found:
        raise SystemExit(f"verify {ts}: {ROOT_NAME}/{ts} not on Drive")
    report = []
    for folder in found:  # normally one; accept any folder that matches fully
        problems = _verify_one(local, folder)
        if not problems:
            total = sum(s for _, s, _ in local)
            log(f"verified {ROOT_NAME}/{ts}: {len(local)} files, {total}B, md5+size match, SHA256SUMS present")
            return
        report += [f"{folder['id']}: {p}" for p in problems]
    for p in report:
        log(f"  {p}")
    raise SystemExit(f"verify {ts}: NOT verified ({len(report)} problem(s) in {len(found)} Drive folder(s))")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("upload"); s.add_argument("local_dir"); s.set_defaults(fn=cmd_upload)
    s = sub.add_parser("list"); s.set_defaults(fn=cmd_list)
    s = sub.add_parser("prune"); s.add_argument("--days", type=int, default=90)
    s.add_argument("--dry-run", action="store_true"); s.set_defaults(fn=cmd_prune)
    s = sub.add_parser("latest"); s.set_defaults(fn=cmd_latest)
    s = sub.add_parser("verify"); s.add_argument("ts", help="<TS> or a path to the local snapshot dir")
    s.add_argument("--local-dir", help="local snapshot dir (default $X_BACKUP_ROOT/<TS>)"); s.set_defaults(fn=cmd_verify)
    s = sub.add_parser("download"); s.add_argument("ts"); s.add_argument("dest_dir"); s.set_defaults(fn=cmd_download)
    a = p.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
