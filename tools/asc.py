#!/usr/bin/env -S uv run --quiet --script
# /// script
# dependencies = ["pyjwt[crypto]", "requests"]
# ///
"""Minimal App Store Connect API client (JWT from an API key).

    asc.py GET  /v1/bundleIds
    asc.py POST /v1/certificates '{"data": {...}}'      (body may also be @file or - for stdin)

Key from ASC_KEY_ID, ASC_ISSUER_ID and ASC_KEY_P8 (a path or the key text) in the environment,
or from ~/.config/apple-developer/account.env when they aren't set. Prints the JSON response.
"""
import json, os, sys, time
import jwt, requests

def env():
    e = {k: os.environ[k] for k in ("ASC_KEY_ID", "ASC_ISSUER_ID", "ASC_KEY_P8") if os.environ.get(k)}
    if len(e) < 3:
        for line in open(os.path.expanduser("~/.config/apple-developer/account.env")):
            if "=" in line and line.startswith("ASC_"):
                k, v = line.strip().split("=", 1); e.setdefault(k, v.strip('"'))
    p8 = e["ASC_KEY_P8"]
    key = p8 if "BEGIN" in p8 else open(os.path.expanduser(p8)).read()
    return e["ASC_KEY_ID"], e["ASC_ISSUER_ID"], key

def token():
    kid, iss, key = env()
    now = int(time.time())
    return jwt.encode({"iss": iss, "iat": now, "exp": now + 1100, "aud": "appstoreconnect-v1"}, key, algorithm="ES256", headers={"kid": kid, "typ": "JWT"})

def call(method, path, body=None):
    url = path if path.startswith("http") else "https://api.appstoreconnect.apple.com" + path
    r = requests.request(method, url, headers={"Authorization": "Bearer " + token(), "Content-Type": "application/json"}, data=body, timeout=60)
    return r.status_code, (r.json() if r.content else {})

if __name__ == "__main__":
    method, path = sys.argv[1].upper(), sys.argv[2]
    body = sys.argv[3] if len(sys.argv) > 3 else None
    if body == "-": body = sys.stdin.read()
    elif body and body.startswith("@"): body = open(body[1:]).read()
    status, j = call(method, path, body)
    print(json.dumps(j, indent=1))
    sys.exit(0 if status < 400 else 1)
