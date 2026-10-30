#!/usr/bin/env python3
"""Copy an original DSH profile's providers to a running dsh-codex instance.

Requires PyYAML. YAML !!js values are never evaluated. Keys travel only in
the authenticated loopback/HTTPS request and are stored by dsh-codex in its
owner-only credentials.toml; output contains ids, counts and key presence.
Existing routes are skipped unless --overwrite is given. The default model
and every unrelated setting are left to the destination.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import re
import shutil
import sys
import urllib.error
import urllib.parse
import urllib.request

import yaml


class ProfileLoader(yaml.SafeLoader):
    pass


# DSH uses YAML 1.2: the effort "off" is a string, not YAML 1.1's false.
ProfileLoader.yaml_implicit_resolvers = {
    key: [(tag, rx) for tag, rx in rows if tag != "tag:yaml.org,2002:bool"]
    for key, rows in ProfileLoader.yaml_implicit_resolvers.items()
}
ProfileLoader.add_implicit_resolver(
    "tag:yaml.org,2002:bool", re.compile(r"^(?:true|false)$", re.I), list("tTfF")
)


class UnexecutedExpression:
    pass


ProfileLoader.add_constructor(
    "tag:yaml.org,2002:js", lambda loader, node: UnexecutedExpression()
)


def load_yaml(path):
    try:
        return yaml.load(path.read_text(), Loader=ProfileLoader)
    except yaml.YAMLError:
        # Parser diagnostics can quote source lines containing credentials.
        raise ValueError(f"Invalid YAML in {path}; source withheld") from None


def plan(dsh_home, profile):
    rows = load_yaml(dsh_home / "profiles" / profile / "cordis.patch.yml")
    providers = next(r["config"]["providers"] for r in rows if r.get("id") == "llm-pi-ai")
    credentials = load_yaml(dsh_home / ".credentials.yaml")
    refs = credentials.get("refs", {})
    result = []
    for provider_id, source in providers.items():
        body = {k: v for k, v in source.items() if k in (
            "api", "baseURL", "apiKeyEnv", "displayName", "titleModel"
        )}
        models = []
        for source_model in source.get("models", []):
            model = {k: v for k, v in source_model.items() if k in (
                "id", "name", "contextWindow", "maxTokens", "input"
            )}
            efforts = source_model.get("reasoningEfforts")
            if isinstance(efforts, dict):
                if any(v != k and not (k == "off" and v is None) for k, v in efforts.items()):
                    raise ValueError(f"{provider_id}/{model['id']}: unsupported custom effort mapping")
                model["reasoningEfforts"] = list(efforts)
            elif efforts is False:
                raise ValueError(f"{provider_id}/{model['id']}: explicit reasoning disable needs manual mapping")
            models.append(model)
        if models:
            body["models"] = models
        env = source.get("apiKeyEnv")
        key = os.environ.get(env) or refs.get(env) if env else None
        if key is not None and not isinstance(key, str):
            raise ValueError(f"{provider_id}: unsupported credential reference")
        if key and key.strip():
            body["apiKey"] = key.strip()
        result.append((provider_id, body))
    return result


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dsh-home", type=Path, default=Path.home() / ".dsh")
    parser.add_argument("--profile", default="desktop")
    parser.add_argument("--ready-file", type=Path)
    parser.add_argument("--url", help="Destination base URL; token from DSHX_WEB_TOKEN")
    parser.add_argument("--backup-home", type=Path, help="Local destination home to back up before writes")
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    imports = plan(args.dsh_home, args.profile)
    if args.dry_run:
        print(json.dumps([{"id": pid, "models": len(body.get("models", [])), "keyPresent": bool(body.get("apiKey"))} for pid, body in imports]))
        return
    if args.ready_file:
        ready = json.loads(args.ready_file.read_text())
        base, token = ready["url"].split("/?")[0].rstrip("/"), ready["token"]
    else:
        base, token = (args.url or "").rstrip("/"), os.environ.get("DSHX_WEB_TOKEN")
    url = urllib.parse.urlsplit(base)
    if not token or not (url.scheme == "https" or (url.scheme == "http" and url.hostname in ("localhost", "127.0.0.1", "::1"))):
        raise ValueError("Supply --ready-file or --url with DSHX_WEB_TOKEN (loopback HTTP or HTTPS)")
    opener = urllib.request.build_opener(NoRedirect())

    def call(method, path, body=None):
        req = urllib.request.Request(base + "/api" + path, method=method,
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
        try:
            with opener.open(req, timeout=45) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise ValueError(f"{method} {path}: HTTP {error.code}; response withheld") from None

    existing = {p["id"] for p in call("GET", "/providers")["providers"]}
    if args.backup_home:
        stamp = datetime.datetime.now().astimezone().strftime("%Y%m%dT%H%M%S%z")
        backup = args.backup_home / "backups" / ("providers-" + stamp)
        backup.mkdir(parents=True, mode=0o700)
        os.chmod(backup.parent, 0o700)
        for filename in ("config.toml", "credentials.toml"):
            source = args.backup_home / filename
            if source.exists():
                target = backup / filename
                shutil.copyfile(source, target)
                os.chmod(target, 0o600)
        print("Backup:", backup)
    for provider_id, body in imports:
        if provider_id in existing and not args.overwrite:
            print(provider_id + ": already configured; skipped")
            continue
        row = call("PUT", "/providers/" + urllib.parse.quote(provider_id, safe=""), body)
        print(json.dumps({"id": provider_id, "models": len(row["models"]), "keyConfigured": row["key"]["configured"]}))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, StopIteration) as error:
        print("Import failed:", str(error), file=sys.stderr)
        sys.exit(1)
