#!/usr/bin/env python3
"""Compare the routes an original declares with the ones Unarr serves.

Audit 3 of ``docs/audit/`` by machine: read the original's API controllers
(a checkout of its source) and Unarr's router table for that personality,
and print the rows that differ.

    scripts/route-audit.py --app radarr --source .cache/src/Radarr/src

What it reads on each side:

- The original: ``*Controller.cs`` under ``<App>.Api.V3`` and the base
  classes in ``<App>.Http``. A controller's prefix comes from
  ``[V3ApiController]`` (the class name without ``Controller``, lower
  case) or from its argument; each ``[HttpGet("sub")]`` and each
  ``[Rest*ById]`` adds a route, the latter with and without the id, as
  ``{id:int?}`` is optional. Actions inherited from ``RestController``
  (``[RestGetById]``) and from ``ProviderControllerBase`` are expanded for
  every subclass.
- Unarr: the ``.route("…", …)`` lines of the personality's router and of
  the shared modules it calls, with the methods each line names.

Paths are compared with their parameters collapsed (``{id}``), case
insensitively, since Unarr lower-cases API paths. A route counted as
present has the same method and path, not necessarily the same answer:
this is the same static comparison the written audit made, no more.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The attributes that declare a route, and the method each one means.
VERBS = {
    "HttpGet": "GET",
    "HttpPost": "POST",
    "HttpPut": "PUT",
    "HttpDelete": "DELETE",
    "HttpPatch": "PATCH",
    "HttpHead": "HEAD",
}
# `Radarr.Http/REST/RestController.cs` and its friends: an action a
# subclass inherits, as (method, path relative to the controller).
REST_BY_ID = {
    "RestGetById": ("GET", "{id}"),
    "RestPostById": ("POST", ""),
    "RestPutById": ("PUT", "{id}"),
    "RestDeleteById": ("DELETE", "{id}"),
}


def collapse(path: str) -> str:
    """`{id:int?}`, `{filename:regex(...)}` and `{name}` all read alike."""
    path = re.sub(r"\{([A-Za-z0-9_]+)[^}]*\}", r"{\1}", path)
    path = re.sub(r"/+", "/", path.strip("/"))
    return path.lower()


def join(prefix: str, rest: str) -> str:
    rest = rest.strip('"').strip()
    if not rest:
        return collapse(prefix)
    return collapse(f"{prefix}/{rest}")


def controller_prefix(text: str, path: Path) -> str | None:
    """The route prefix of a controller, or None when it declares none."""
    match = re.search(r"\[V3ApiController(?:\(\"([^\"]*)\"\))?\]", text)
    if not match:
        return None
    if match.group(1):
        return match.group(1)
    name = re.search(r"class\s+(\w+?)Controller\b", text)
    return name.group(1).lower() if name else path.stem.replace("Controller", "").lower()


def base_classes(text: str) -> list[str]:
    match = re.search(r"class\s+\w+\s*:\s*([^\{]+)\{", text, re.S)
    if not match:
        return []
    return [b.strip().split("<")[0].strip() for b in match.group(1).split(",")]


def inherited_routes(bases: list[str], inherited: dict[str, list[tuple[str, str]]]) -> list[tuple[str, str]]:
    out: list[tuple[str, str]] = []
    for base in bases:
        out.extend(inherited.get(base, []))
    return out


def routes_of(text: str, prefix: str) -> list[tuple[str, str]]:
    """The routes a controller's own attributes declare."""
    out: list[tuple[str, str]] = []
    for attribute, verb in VERBS.items():
        for match in re.finditer(rf"\[{attribute}(?:\(([^\)]*)\))?\]", text):
            argument = (match.group(1) or "").strip()
            # [HttpGet(Name = "...")] and other named arguments route the
            # action at the controller's own path.
            if "=" in argument and not argument.startswith('"'):
                argument = ""
            out.append((verb, join(prefix, argument)))
    for attribute, (verb, rest) in REST_BY_ID.items():
        if re.search(rf"\[{attribute}\]", text):
            out.append((verb, join(prefix, rest)))
            if rest:
                # `{id:int?}`: the id-less form is a route of its own.
                out.append((verb, join(prefix, "")))
    return out


def original_routes(source: Path, app: str) -> dict[tuple[str, str], str]:
    """Every (method, path) the original declares, with where it is from."""
    api = source / f"{app.capitalize()}.Api.V3"
    http = source / f"{app.capitalize()}.Http"
    if not api.is_dir():
        sys.exit(f"{api} is not there: give --source the `src` directory of a checkout")
    # The actions a base class gives its subclasses.
    inherited: dict[str, list[tuple[str, str]]] = {}
    for path in list(http.rglob("*.cs")) + list(api.rglob("*.cs")):
        text = path.read_text(encoding="utf-8", errors="replace")
        for name in re.findall(r"(?:abstract\s+)?class\s+(\w+)", text):
            own = []
            for attribute, (verb, rest) in REST_BY_ID.items():
                if re.search(rf"\[{attribute}\]", text):
                    own.append((verb, rest))
            for attribute, verb in VERBS.items():
                for match in re.finditer(rf"\[{attribute}(?:\(\"([^\"]*)\"\))?\]", text):
                    own.append((verb, match.group(1) or ""))
            # Every class of `<App>.Http` is a base class of controllers;
            # in the API tree only those that say so by name or keyword.
            is_base = http in path.parents or "Base" in name or "abstract" in text.split(f"class {name}")[0][-60:]
            if own and is_base:
                inherited[name] = own
    out: dict[tuple[str, str], str] = {}
    for path in sorted(api.rglob("*Controller.cs")):
        text = path.read_text(encoding="utf-8", errors="replace")
        prefix = controller_prefix(text, path)
        if prefix is None:
            continue
        where = str(path.relative_to(source))
        for verb, route in routes_of(text, prefix):
            out.setdefault((verb, route), where)
        for verb, rest in inherited_routes(base_classes(text), inherited):
            out.setdefault((verb, join(prefix, rest)), f"{where} (inherited)")
            if rest.startswith("{id}"):
                out.setdefault((verb, join(prefix, "")), f"{where} (inherited)")
    return out


ROUTE_LINE = re.compile(r"\.route\(\s*\"([^\"]+)\"\s*,", re.S)


def unarr_routes(app: str) -> dict[tuple[str, str], str]:
    """Every (method, path) Unarr's router declares for this personality."""
    api = REPO / "crates" / "unarr-api" / "src" / "routes"
    files = [api / "radarr" / "mod.rs"] if app == "radarr" else [api / "mod.rs"]
    out: dict[tuple[str, str], str] = {}
    for file in files:
        text = file.read_text()
        # Each `.route("…", …)` call, up to the next one.
        starts = [m for m in ROUTE_LINE.finditer(text)]
        for i, match in enumerate(starts):
            end = starts[i + 1].start() if i + 1 < len(starts) else len(text)
            body = text[match.end():end]
            path = match.group(1)
            methods = set()
            for verb in ("get", "post", "put", "delete", "patch", "head"):
                if re.search(rf"\b{verb}\(", body):
                    methods.add(verb.upper())
            if re.search(r"\bany\(", body):
                methods.update({"GET", "POST", "PUT", "DELETE"})
            for method in methods:
                out.setdefault((method, collapse(path.removeprefix("/api/v3"))), str(file.relative_to(REPO)))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--app", choices=["radarr", "sonarr"], default="radarr")
    ap.add_argument("--source", default=".cache/src/Radarr/src", help="the original's `src` directory")
    ap.add_argument("--all", action="store_true", help="list the routes both sides have too")
    args = ap.parse_args()

    original = original_routes(Path(args.source), args.app)
    ours = unarr_routes(args.app)
    # `[RestPutById]` and `[RestDeleteById]` are `{id:int?}`: Unarr serves
    # the id-less form of a `PUT` by rewriting the path from the body's id
    # (`lib.rs`), so it counts as present where the id form is served.
    def served(key: tuple[str, str]) -> bool:
        verb, path = key
        if key in ours:
            return True
        return verb == "PUT" and (verb, f"{path}/{{id}}") in ours

    missing = sorted(k for k in original if not served(k))
    extra = sorted(k for k in ours if k not in original and (k[0], k[1].rsplit("/{", 1)[0]) not in original)
    print(f"{len(original)} routes declared by the original, {len(ours)} served by Unarr")
    print(f"{len(missing)} missing, {len(extra)} Unarr has and the original does not\n")
    if missing:
        print("MISSING (the original declares, Unarr does not):")
        for verb, path in missing:
            print(f"  {verb:6} /{path:48} {original[(verb, path)]}")
    if extra:
        print("\nEXTRA (Unarr serves, the original's controllers do not declare):")
        for verb, path in extra:
            print(f"  {verb:6} /{path}")
    if args.all:
        print("\nPRESENT:")
        for verb, path in sorted(k for k in original if k in ours):
            print(f"  {verb:6} /{path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
