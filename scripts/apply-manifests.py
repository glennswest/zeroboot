#!/usr/bin/env python3
"""apply-manifests.py — apply YAML manifests to a rustkube apiserver.

The stormcos node has no kubectl (immutable image, no package manager), so the
bootstrap phase applies manifests by POSTing them to the apiserver's REST API
directly. In DEV mode the apiserver serves plain HTTP with --anonymous-auth, so
no client cert is needed (see bootstrap-cluster.sh; revisit once
rustkube-node#19 lands and we go back to TLS).

Create-or-replace, not a real server-side apply: POST, and on 409 Conflict GET
the live object, carry its resourceVersion into ours, and PUT. That is enough
for bootstrap (the network-operator does its own SSA reconciliation afterwards)
and keeps the script re-runnable.

Usage: apply-manifests.py <apiserver-url> <file-or-dir> [<file-or-dir> ...]
"""

import json
import sys
import os
import glob
import urllib.request
import urllib.error

try:
    import yaml
except ImportError:
    sys.exit("PyYAML not available (it ships with cloud-init on the node)")

# kind -> (api path prefix, plural, namespaced). Derived from apiVersion at
# runtime; only the plural and scope need a table.
PLURALS = {
    "ServiceAccount": ("serviceaccounts", True),
    "ConfigMap": ("configmaps", True),
    "Secret": ("secrets", True),
    "Service": ("services", True),
    "Deployment": ("deployments", True),
    "DaemonSet": ("daemonsets", True),
    "ClusterRole": ("clusterroles", False),
    "ClusterRoleBinding": ("clusterrolebindings", False),
    "Role": ("roles", True),
    "RoleBinding": ("rolebindings", True),
    "CustomResourceDefinition": ("customresourcedefinitions", False),
    "Network": ("networks", False),
    "Namespace": ("namespaces", False),
}


def resource_path(base, obj):
    """REST collection URL for an object, from apiVersion + kind."""
    api_version = obj["apiVersion"]
    kind = obj["kind"]
    if kind not in PLURALS:
        raise SystemExit(f"unhandled kind {kind!r} — add it to PLURALS")
    plural, namespaced = PLURALS[kind]

    # core group is /api/v1; everything else is /apis/<group>/<version>
    if "/" in api_version:
        root = f"{base}/apis/{api_version}"
    else:
        root = f"{base}/api/{api_version}"

    if namespaced:
        ns = obj.get("metadata", {}).get("namespace", "default")
        return f"{root}/namespaces/{ns}/{plural}"
    return f"{root}/{plural}"


def req(url, method="GET", body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    r.add_header("Content-Type", "application/json")
    r.add_header("Accept", "application/json")
    with urllib.request.urlopen(r, timeout=30) as resp:
        return resp.status, json.loads(resp.read() or b"{}")


def apply_one(base, obj):
    kind = obj["kind"]
    name = obj.get("metadata", {}).get("name", "<unnamed>")
    coll = resource_path(base, obj)
    try:
        req(coll, "POST", obj)
        print(f"  created  {kind}/{name}")
        return True
    except urllib.error.HTTPError as e:
        if e.code != 409:
            print(f"  FAILED   {kind}/{name}: HTTP {e.code} {e.read()[:200]!r}")
            return False
    # Already exists: carry the live resourceVersion and replace.
    try:
        _, live = req(f"{coll}/{name}")
        obj.setdefault("metadata", {})["resourceVersion"] = (
            live.get("metadata", {}).get("resourceVersion")
        )
        req(f"{coll}/{name}", "PUT", obj)
        print(f"  replaced {kind}/{name}")
        return True
    except urllib.error.HTTPError as e:
        print(f"  FAILED   {kind}/{name}: HTTP {e.code} {e.read()[:200]!r}")
        return False


def docs(paths):
    for p in paths:
        files = (
            sorted(glob.glob(os.path.join(p, "*.yaml")) + glob.glob(os.path.join(p, "*.yml")))
            if os.path.isdir(p)
            else [p]
        )
        for f in files:
            with open(f) as fh:
                for d in yaml.safe_load_all(fh):
                    if d:
                        yield f, d


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    base = sys.argv[1].rstrip("/")
    failures = 0
    for src, obj in docs(sys.argv[2:]):
        if not apply_one(base, obj):
            failures += 1
    if failures:
        sys.exit(f"{failures} manifest(s) failed")
    print("all manifests applied")


if __name__ == "__main__":
    main()
