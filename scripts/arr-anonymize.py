#!/usr/bin/env python3
"""Make an anonymised copy of a Sonarr or Radarr installation's config.xml and database.

Input is either a backup zip (System > Backup > Backup Now; it holds a
consistent snapshot of both files) or a directory with `config.xml` and
`sonarr.db` (plus `sonarr.db-wal` / `sonarr.db-shm` if Sonarr was writing);
`radarr.db` with `--app radarr`. Output is a directory with the two files,
ready to drop into `tests/fixtures/sonarr/` or `tests/fixtures/radarr/`.

    scripts/arr-anonymize.py --from sonarr_backup_v4.0.20.3014_2026.11.07_10.00.00.zip --out .cache/anonymized
    scripts/arr-anonymize.py --app radarr --from radarr_backup_v6.4.4.10685_2026.11.11_12.07.09.zip --out .cache/anonymized

What is removed or replaced, keeping every shape the API exposes:

- config.xml: new API key (random, or `--api-key`), SSL certificate path and
  password blanked, allowed hosts and trusted networks reset,
  Postgres credentials removed (if present), instance name kept unless
  `--instance-name` is given.
- Users: replaced by one forms-auth user `unarr` / `fixture`, so the copy can
  be logged into without the real password hash.
- Config: the Rijndael and HMAC passphrases and salts and the Plex client
  identifier regenerated; paths rewritten like everything else.
- Indexers, DownloadClients, Notifications, ImportLists, Metadata: `Name`
  replaced by the implementation, `Settings` JSON walked and every credential,
  URL, host, e-mail, token, chat/device/user identifier replaced by a
  placeholder of the same type.
- History, DownloadHistory, Blocklist, PendingReleases: download URLs, info
  URLs, indexer and download client names, failure messages redacted; release
  GUIDs, torrent hashes and download client ids replaced by stable pseudonyms
  (rows that referred to the same release still do); paths rewritten.
- Root folders and every path under them rewritten to `/tv`, `/tv2`, ...
  (`/movies`, `/movies2`, ... for Radarr, collections' root folders included);
  paths outside a root folder (download folders) reduced to their last
  component under `/downloads`. Remote path mappings normalised.
- Commands and the provider status tables emptied (stack traces, last RSS
  releases). The database is then checkpointed, vacuumed and integrity-checked.
- Movies, movie files, credits, collections, translations and list items
  are public data and stay as they are.
- Release names (`SceneName`, `ReleaseGroup`, `SourceTitle`) are kept by
  default: they are public and the parser needs them. `--scrub-releases` blanks
  them.

Before writing, the script re-reads the whole output database and fails if any
original secret, path prefix, user name or API key is still present.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import secrets
import shutil
import sqlite3
import sys
import tempfile
import uuid
import zipfile
from pathlib import Path, PurePosixPath, PureWindowsPath

PROVIDER_TABLES = ["Indexers", "DownloadClients", "Notifications", "ImportLists", "Metadata"]
STATUS_TABLES = ["IndexerStatus", "DownloadClientStatus", "ImportListStatus", "NotificationStatus"]

# Settings keys whose string values are replaced. Matched case-insensitively
# against the key name; drawn from the FieldDefinition privacy levels in the
# Sonarr source plus the fields that hold addresses and identifiers.
SECRET_KEY = re.compile(
    r"(token|key|secret|pass|user|login|auth|chat|device|channel|room|recipient|"
    r"account|email|mail|^to$|^from$|^cc$|^bcc$|address|host|url|server|webhook|"
    r"phone|number|^id$|.id$|ids$|path$|folder$|dir$|^topic$|^sender|author|avatar)",
    re.IGNORECASE,
)
# Keys that are plain configuration and must survive even if they match above.
SAFE_KEY = re.compile(
    r"^(port|useSsl|apiPath|categories|animeCategories|animeStandardFormatSearch|"
    r"multiLanguages|failDownloads|tvCategory|tvDirectory|recentTvPriority|olderTvPriority|"
    r"movieCategory|movieDirectory|recentMoviePriority|olderMoviePriority|"
    r"movieMetadata|movieMetadataURL|movieMetadataLanguage|movieImages|useMovieNfo|"
    r"addCollectionName|removeAccents|removeYear|requiredFlags|"
    r"addPaused|initialState|sequentialOrder|firstAndLast|contentLayout|method|"
    r"priority|retention|minimumSeeders|seedCriteria|seedRatio|seedTime|seasonPackSeedTime|"
    r"rejectBlocklistedTorrentHashesWhileGrabbing|removeCompletedDownloads|"
    r"removeFailedDownloads|headers|grabFields|importFields|includeHealthWarnings|"
    r"seriesMetadata|seriesMetadataUrl|seriesMetadataEpisodeGuide|seriesImages|"
    r"seasonImages|episodeMetadata|episodeImages|episodeMetadataUrl|"
    r"episodeMetadataThumbnail|seriesMetadataEpisodeGuide|limit|requireTorrentFiles|"
    r"updateLibrary|notifyOnPlay|mapFrom|mapTo|sound|expire|"
    r"magnetFileExtension|isTorrent|readyForUpload|exclusive|urlBase|language)$",
    re.IGNORECASE,
)
URL_RE = re.compile(r"^[a-z][a-z0-9+.-]*://", re.IGNORECASE)
# What this script writes; an original that already looks like it is not tracked.
PLACEHOLDER_RE = re.compile(r"\.invalid(/|$)|^redacted$|^/(tv|movies)\d*(/|$)|^/downloads\d*(/|$)|^/remote\d+/")
EMAIL_RE = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")
# The C# source adds these in PascalCase; the stored JSON is camelCase. Compared lowercased.
HISTORY_URL_KEYS = {"downloadurl", "guid", "nzbinfourl", "infourl", "magneturl"}
HISTORY_PATH_KEYS = {"droppedpath", "importedpath", "sourcepath", "destinationpath", "path", "sourcerelativepath", "relativepath"}
HISTORY_TEXT_KEYS = {"indexer": "Indexer", "downloadclientname": "Download client", "message": "redacted"}
HISTORY_RELEASE_KEYS = {"releasegroup", "sourcetitle", "title"}
# Opaque release identifiers: indexer GUIDs that are not URLs, torrent info
# hashes, download client ids. Replaced by a pseudonym of the same length, so
# rows that referred to the same release still do.
TOKEN_KEYS = {"guid", "torrentinfohash", "infohash", "downloadid", "releasehash", "hash"}
HEX_TOKEN_RE = re.compile(r"^[0-9a-fA-F]{24,}$")

# What differs between the applications: the database file, the library
# root the paths are rewritten to, the table holding the library's folders
# and the one holding its files.
APPS = {
    "sonarr": {"name": "Sonarr", "db": "sonarr.db", "root": "/tv", "library": "Series", "files": "EpisodeFiles", "file": "episode file"},
    "radarr": {"name": "Radarr", "db": "radarr.db", "root": "/movies", "library": "Movies", "files": "MovieFiles", "file": "movie file"},
}


class Anonymizer:
    def __init__(self, con: sqlite3.Connection, scrub_releases: bool, app: dict = APPS["sonarr"]):
        self.con = con
        self.app = app
        self.scrub_releases = scrub_releases
        self.secrets: dict[str, str] = {}  # original value -> kind; none may survive
        self.report: dict[str, int] = {}
        self.tables = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        self.root_map: dict[str, str] = {}
        # Implementation and contract names stay in the output; a provider the
        # user named exactly like its implementation ("qBittorrent") is not a
        # value to chase.
        self.implementations: set[str] = set()
        for table in PROVIDER_TABLES:
            if self.has(table, "Implementation", "ConfigContract"):
                for impl, contract in con.execute(f"SELECT Implementation, ConfigContract FROM {table}"):
                    for name in (impl, contract):
                        if name:
                            self.implementations.add(name.lower())
                            self.implementations.add(name.lower().removesuffix("settings"))

    # -- helpers -----------------------------------------------------------
    def has(self, table: str, *columns: str) -> bool:
        if table not in self.tables:
            return False
        cols = {r[1] for r in self.con.execute(f"PRAGMA table_info({table})")}
        return all(c in cols for c in columns)

    def count(self, key: str, n: int = 1):
        self.report[key] = self.report.get(key, 0) + n

    def forget(self, value, kind="credential"):
        """Remember an original value so the final scan can prove it is gone."""
        # Short values (user names, labels) are replaced all the same but not
        # scanned for: a common word would show up legitimately elsewhere.
        if isinstance(value, str) and len(value) >= 8 and not value.isdigit():
            if PLACEHOLDER_RE.search(value):
                return  # already looks like what this script writes
            if kind == "provider name" and value.lower() in self.implementations:
                return
            if URL_RE.match(value):
                # Thousands of download URLs share a few hosts: remember the host.
                m = re.match(r"^([a-z][a-z0-9+.-]*://[^/?#]+)", value, re.IGNORECASE)
                value = m.group(1) if m else value
                kind = "url"
            self.secrets.setdefault(value, kind)

    def forget_path(self, path: str):
        """Remember the first components of a path (where user and share names live)."""
        sep = "\\" if self.is_windows(path) and "\\" in path else "/"
        parts = [c for c in re.split(r"[\\/]", path) if c]
        if not parts:
            return
        prefix = sep.join(parts[:3])
        if not self.is_windows(path):
            prefix = "/" + prefix
        self.forget(prefix, "path")

    @staticmethod
    def is_windows(path: str) -> bool:
        return bool(re.match(r"^[A-Za-z]:[\\/]", path)) or path.startswith("\\\\")

    def rewrite_path(self, path):
        """Map a path under a root folder to /tvN/... (/moviesN/...); anything else to /downloads/<last>."""
        if not isinstance(path, str) or not path:
            return path
        norm = path.replace("\\", "/") if self.is_windows(path) else path
        for old, new in self.root_map.items():
            old_norm = old.replace("\\", "/") if self.is_windows(old) else old
            if norm.rstrip("/").lower() == old_norm.rstrip("/").lower():
                return new + ("/" if norm.endswith("/") else "")
            prefix = old_norm.rstrip("/") + "/"
            if norm.lower().startswith(prefix.lower()):
                return new + "/" + norm[len(prefix):]
        last = (PureWindowsPath(path) if self.is_windows(path) else PurePosixPath(path)).name
        new = "/downloads/" + last if last else "/downloads"
        if new != path:
            self.forget_path(path)
        return new

    # -- steps -------------------------------------------------------------
    def root_folders(self):
        if not self.has("RootFolders", "Path"):
            return
        rows = self.con.execute("SELECT Id, Path FROM RootFolders ORDER BY Id").fetchall()
        for n, (id_, path) in enumerate(rows, start=1):
            new = self.app["root"] if n == 1 else f"{self.app['root']}{n}"
            self.root_map[path] = new
            self.forget(path.rstrip("/\\"), "root folder")
            self.con.execute("UPDATE RootFolders SET Path=? WHERE Id=?", (new, id_))
            self.count("root folders renamed")

    def library_paths(self):
        library = self.app["library"]
        if not self.has(library, "Path"):
            return
        for id_, path in self.con.execute(f"SELECT Id, Path FROM {library}").fetchall():
            self.con.execute(f"UPDATE {library} SET Path=? WHERE Id=?", (self.rewrite_path(path), id_))
            self.count(f"{library.lower()} paths rewritten")
        # Radarr's collections remember the root folder their movies go to.
        for table in ("ImportLists", "Collections"):
            if self.has(table, "RootFolderPath"):
                for id_, path in self.con.execute(f"SELECT Id, RootFolderPath FROM {table}").fetchall():
                    self.con.execute(f"UPDATE {table} SET RootFolderPath=? WHERE Id=?", (self.rewrite_path(path), id_))

    def users(self):
        if not self.has("Users", "Identifier", "Username", "Password", "Salt", "Iterations"):
            return
        for username, password in self.con.execute("SELECT Username, Password FROM Users"):
            self.forget(username, "user name")
            self.forget(password, "password hash")
        self.con.execute("DELETE FROM Users")
        salt = os.urandom(16)
        hashed = hashlib.pbkdf2_hmac("sha512", b"fixture", salt, 10000, 32)
        self.con.execute(
            "INSERT INTO Users (Identifier, Username, Password, Salt, Iterations) VALUES (?, ?, ?, ?, ?)",
            (str(uuid.uuid4()), "unarr", base64.b64encode(hashed).decode(), base64.b64encode(salt).decode(), 10000),
        )
        self.count("users replaced")

    def config_table(self):
        if not self.has("Config", "Key", "Value"):
            return
        for id_, key, value in self.con.execute("SELECT Id, Key, Value FROM Config").fetchall():
            new = value
            if key in ("rijndaelpassphrase", "rijndaelsalt", "hmacpassphrase", "hmacsalt", "plexclientidentifier"):
                self.forget(value, "config secret")
                new = str(uuid.uuid4())
            elif isinstance(value, str) and (value.startswith("/") or self.is_windows(value)):
                new = self.rewrite_path(value)
            if new != value:
                self.con.execute("UPDATE Config SET Value=? WHERE Id=?", (new, id_))
                self.count("config values replaced")

    def scrub_json(self, node, implementation: str, path_keys=frozenset()):
        """Walk a settings/release JSON tree replacing sensitive leaves in place."""
        if isinstance(node, dict):
            for key, value in list(node.items()):
                if isinstance(value, (dict, list)):
                    self.scrub_json(value, implementation, path_keys)
                    continue
                if not isinstance(value, str) or value == "":
                    continue
                if key.lower() in path_keys or value.startswith("/") or self.is_windows(value):
                    node[key] = self.rewrite_path(value)
                elif URL_RE.match(value):
                    self.forget(value)
                    node[key] = f"http://{implementation.lower()}.invalid/"
                elif EMAIL_RE.match(value):
                    self.forget(value, "e-mail")
                    node[key] = "user@example.invalid"
                elif re.search(r"indexer$|downloadclient", key, re.IGNORECASE):
                    self.forget(value, "provider name")
                    node[key] = "redacted"
                elif SECRET_KEY.search(key) and not SAFE_KEY.match(key):
                    self.forget(value)
                    node[key] = f"{implementation.lower()}.invalid" if re.search("host|server", key, re.I) else "redacted"
        elif isinstance(node, list):
            for i, item in enumerate(node):
                if isinstance(item, (dict, list)):
                    self.scrub_json(item, implementation, path_keys)
                elif isinstance(item, str) and (EMAIL_RE.match(item) or URL_RE.match(item)):
                    self.forget(item, "e-mail" if EMAIL_RE.match(item) else "url")
                    node[i] = "user@example.invalid" if EMAIL_RE.match(item) else f"http://{implementation.lower()}.invalid/"

    def providers(self):
        for table in PROVIDER_TABLES:
            if not self.has(table, "Name", "Implementation", "Settings"):
                continue
            rows = self.con.execute(f"SELECT Id, Name, Implementation, Settings FROM {table}").fetchall()
            for id_, name, implementation, settings in rows:
                impl = implementation or table[:-1]
                try:
                    tree = json.loads(settings) if settings else {}
                except json.JSONDecodeError:
                    tree = {}
                self.scrub_json(tree, impl)
                self.con.execute(
                    f"UPDATE {table} SET Name=?, Settings=? WHERE Id=?",
                    (f"{impl} {id_}", json.dumps(tree, indent=2), id_),
                )
                self.count(f"{table} scrubbed")
        for table in STATUS_TABLES + ["Commands"]:
            if table in self.tables:
                n = self.con.execute(f"DELETE FROM {table}").rowcount
                if n:
                    self.count(f"{table} rows deleted", n)

    @staticmethod
    def pseudonym(value: str) -> str:
        """A stable stand-in of the same length for an opaque identifier."""
        digest = hashlib.sha256(value.encode()).hexdigest()
        return (digest * 4)[: max(12, len(value))]

    def token(self, value, kind="release id"):
        """Pseudonymise a release/download identifier column value."""
        if not isinstance(value, str) or len(value) < 8:
            return value
        self.forget(value, kind)
        return self.pseudonym(value)

    def scrub_history_data(self, raw, implementation="indexer"):
        """Scrub a history `Data`, a `ReleaseInfo` or a pending release JSON object.

        Keys are matched case-insensitively (the stored JSON is camelCase, the
        C# source PascalCase). A second pass removes any value replaced in the
        first one from the remaining strings, so an identifier that also appears
        inside another field (a GUID in an info URL, a hash in a message) goes
        with it.
        """
        try:
            data = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            return raw
        if not isinstance(data, dict):
            return raw
        replaced: dict[str, str] = {}
        for key, value in list(data.items()):
            if not isinstance(value, str) or value == "":
                continue
            k = key.lower()
            if value.lower().startswith("magnet:"):
                self.forget(value, "url")
                data[key] = "magnet:?xt=urn:btih:" + self.pseudonym(value)[:40]
            elif URL_RE.match(value):
                self.forget(value, "url")
                data[key] = f"http://{implementation}.invalid/{self.pseudonym(value)[:12]}"
            elif k in HISTORY_URL_KEYS or k in TOKEN_KEYS or HEX_TOKEN_RE.match(value):
                if len(value) >= 8:
                    self.forget(value, "release id")
                    data[key] = self.pseudonym(value)
            elif k in HISTORY_PATH_KEYS or value.startswith("/") or self.is_windows(value):
                data[key] = self.rewrite_path(value)
            elif k in HISTORY_TEXT_KEYS:
                self.forget(value, "provider name" if k != "message" else "message")
                data[key] = HISTORY_TEXT_KEYS[k]
            elif self.scrub_releases and k in HISTORY_RELEASE_KEYS:
                data[key] = ""
            if data[key] != value and len(value) >= 8:
                replaced[value] = data[key] if k in TOKEN_KEYS or HEX_TOKEN_RE.match(value) else "redacted"
        for key, value in list(data.items()):
            if isinstance(value, str):
                for old, new in replaced.items():
                    if old in value:
                        value = value.replace(old, new)
                data[key] = value
        return json.dumps(data, indent=2)

    def source_titles(self, table: str):
        """SourceTitle is a release name, or a full path for file deletion events."""
        if not self.has(table, "SourceTitle"):
            return
        if self.scrub_releases:
            self.con.execute(f"UPDATE {table} SET SourceTitle=''")
            return
        for id_, title in self.con.execute(f"SELECT Id, SourceTitle FROM {table}").fetchall():
            if isinstance(title, str) and (title.startswith("/") or self.is_windows(title)):
                self.con.execute(f"UPDATE {table} SET SourceTitle=? WHERE Id=?", (self.rewrite_path(title), id_))
                self.count(f"{table} path titles rewritten")

    def history(self):
        if self.has("History", "Data"):
            has_download_id = self.has("History", "DownloadId")
            cols = "Id, Data, DownloadId" if has_download_id else "Id, Data, NULL"
            for id_, data, download_id in self.con.execute(f"SELECT {cols} FROM History").fetchall():
                self.con.execute("UPDATE History SET Data=? WHERE Id=?", (self.scrub_history_data(data), id_))
                if has_download_id:
                    self.con.execute("UPDATE History SET DownloadId=? WHERE Id=?", (self.token(download_id), id_))
                self.count("history rows scrubbed")
            self.source_titles("History")
        if self.has("DownloadHistory", "Data", "Release", "DownloadId"):
            for id_, data, release, download_id in self.con.execute(
                "SELECT Id, Data, Release, DownloadId FROM DownloadHistory"
            ).fetchall():
                self.con.execute(
                    "UPDATE DownloadHistory SET Data=?, Release=?, DownloadId=? WHERE Id=?",
                    (self.scrub_history_data(data), self.scrub_history_data(release), self.token(download_id), id_),
                )
                self.count("download history rows scrubbed")
            self.source_titles("DownloadHistory")
        if self.has("Blocklist", "Indexer", "Message"):
            has_hash = self.has("Blocklist", "TorrentInfoHash")
            for id_, indexer, message, info_hash in self.con.execute(
                "SELECT Id, Indexer, Message, " + ("TorrentInfoHash" if has_hash else "NULL") + " FROM Blocklist"
            ).fetchall():
                self.forget(indexer, "provider name")
                self.forget(message, "message")
                self.con.execute("UPDATE Blocklist SET Indexer='Indexer', Message='redacted' WHERE Id=?", (id_,))
                if has_hash:
                    self.con.execute("UPDATE Blocklist SET TorrentInfoHash=? WHERE Id=?", (self.token(info_hash), id_))
                self.count("blocklist rows scrubbed")
            self.source_titles("Blocklist")
        if self.has("PendingReleases", "Release"):
            for id_, release in self.con.execute("SELECT Id, Release FROM PendingReleases").fetchall():
                self.con.execute(
                    "UPDATE PendingReleases SET Release=? WHERE Id=?", (self.scrub_history_data(release), id_)
                )
                self.count("pending releases scrubbed")

    def files(self):
        if self.has("AutoTagging", "Specifications"):
            for id_, specs in self.con.execute("SELECT Id, Specifications FROM AutoTagging").fetchall():
                try:
                    tree = json.loads(specs) if specs else None
                except json.JSONDecodeError:
                    continue
                if tree is None:
                    continue
                self.scrub_json(tree, "autotagging")
                self.con.execute("UPDATE AutoTagging SET Specifications=? WHERE Id=?", (json.dumps(tree, indent=2), id_))
                self.count("auto tagging rules scrubbed")
        if self.has("RemotePathMappings", "Host", "RemotePath", "LocalPath"):
            for id_, host, remote, local in self.con.execute("SELECT Id, Host, RemotePath, LocalPath FROM RemotePathMappings").fetchall():
                self.forget(host, "host")
                self.con.execute(
                    "UPDATE RemotePathMappings SET Host=?, RemotePath=?, LocalPath=? WHERE Id=?",
                    ("downloadclient.invalid", f"/remote{id_}/", f"/downloads{id_}/", id_),
                )
                self.count("remote path mappings rewritten")
        files, kind = self.app["files"], self.app["file"]
        if self.has(files, "MediaInfo"):
            # The raw ffprobe output keeps the full file name; the parsed fields do not.
            for id_, raw in self.con.execute(f"SELECT Id, MediaInfo FROM {files}").fetchall():
                try:
                    info = json.loads(raw) if raw else None
                except json.JSONDecodeError:
                    continue
                if isinstance(info, dict) and "rawStreamData" in info:
                    del info["rawStreamData"]
                    self.con.execute(f"UPDATE {files} SET MediaInfo=? WHERE Id=?", (json.dumps(info, indent=2), id_))
                    self.count(f"{kind} raw ffprobe data dropped")
        if self.has(files, "OriginalFilePath"):
            # Usually "<release folder>/<file>"; absolute when imported from a loose file.
            for id_, original in self.con.execute(f"SELECT Id, OriginalFilePath FROM {files}").fetchall():
                if isinstance(original, str) and (original.startswith("/") or self.is_windows(original)):
                    self.forget_path(original)
                    name = (PureWindowsPath(original) if self.is_windows(original) else PurePosixPath(original)).name
                    self.con.execute(f"UPDATE {files} SET OriginalFilePath=? WHERE Id=?", (name, id_))
                    self.count(f"{kind} original paths reduced")
        if self.scrub_releases and self.has(files, "SceneName", "ReleaseGroup", "OriginalFilePath"):
            self.con.execute(f"UPDATE {files} SET SceneName=NULL, ReleaseGroup=NULL, OriginalFilePath=NULL")
            self.count(f"{kind} release names blanked")

    def run(self):
        self.root_folders()
        self.library_paths()
        self.users()
        self.config_table()
        self.providers()
        self.history()
        self.files()
        self.con.commit()

    def verify(self, extra: dict[str, str]) -> list[tuple[str, str, list[str]]]:
        """Return the remembered original values that survive, with where they are.

        Each entry is (value, kind, ["Table.Column id=..", ...]).
        """
        needles = dict(self.secrets)
        needles.update({v: k for v, k in extra.items() if v})
        lowered = "\n".join(self.con.iterdump()).lower()
        leaked = []
        for value, kind in sorted(needles.items()):
            if value.lower() not in lowered:
                continue
            where = []
            for table in sorted(self.tables - {"sqlite_sequence"}):
                cols = [r[1] for r in self.con.execute(f'PRAGMA table_info("{table}")')]
                key = '"Id"' if "Id" in cols else "rowid"
                for col in cols:
                    rows = self.con.execute(
                        f'SELECT {key} FROM "{table}" WHERE instr(lower(CAST("{col}" AS TEXT)), ?) > 0 LIMIT 5',
                        (value.lower(),),
                    ).fetchall()
                    if rows:
                        where.append(f"{table}.{col} {key}={','.join(str(r[0]) for r in rows)}")
            leaked.append((value, kind, where or ["(schema or index text)"]))
        return leaked


def random_api_key() -> str:
    return secrets.token_hex(16)


def anonymize_config_xml(text: str, new_key: str, instance_name: str | None) -> tuple[str, dict[str, str]]:
    found: dict[str, str] = {}

    def replace(tag: str, value: str | None):
        nonlocal text
        m = re.search(rf"<{tag}>(.*?)</{tag}>", text, re.DOTALL)
        if not m:
            return
        found[tag] = m.group(1)
        if value is None:
            text = text[: m.start()] + text[m.end():]
            text = re.sub(r"\n[ \t]*\n", "\n", text)
        else:
            text = text[: m.start(1)] + value + text[m.end(1):]

    replace("ApiKey", new_key)
    replace("SslCertPath", "")
    replace("SslCertPassword", "")
    # Host names and subnets of the LAN.
    replace("AllowedHosts", "*")
    replace("TrustedNetworks", "")
    for tag in ("PostgresUser", "PostgresPassword", "PostgresHost", "PostgresPort", "PostgresMainDb", "PostgresLogDb"):
        replace(tag, None)
    if instance_name is not None:
        replace("InstanceName", instance_name)
    return text, found


def locate_inputs(source: Path, work: Path, app: dict) -> tuple[Path, Path]:
    """Copy config.xml and the database (with WAL side files) from a zip or a directory into `work`."""
    db, name = app["db"], app["name"]
    if source.is_file() and zipfile.is_zipfile(source):
        with zipfile.ZipFile(source) as z:
            names = {Path(n).name.lower(): n for n in z.namelist()}
            for wanted in ("config.xml", db):
                if wanted not in names:
                    sys.exit(f"{source}: no {wanted} inside the zip")
                with z.open(names[wanted]) as src, open(work / wanted, "wb") as dst:
                    shutil.copyfileobj(src, dst)
    elif source.is_dir():
        for wanted in ("config.xml", db):
            if not (source / wanted).exists():
                sys.exit(f"{source}: no {wanted}. If {name} uses PostgreSQL there is no {db} to anonymise.")
            shutil.copy2(source / wanted, work / wanted)
        for side in (db + "-wal", db + "-shm"):
            if (source / side).exists():
                shutil.copy2(source / side, work / side)
    else:
        sys.exit(f"{source}: expected a {name} backup zip or a directory with config.xml and {db}")
    return work / "config.xml", work / db


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--app", choices=sorted(APPS), default="sonarr", help="the application the backup is from (default: sonarr)")
    ap.add_argument("--from", dest="source", required=True, type=Path, help="backup zip, or a directory with config.xml and the database")
    ap.add_argument("--out", required=True, type=Path, help="output directory (created; must be empty)")
    ap.add_argument("--instance-name", help="replace <InstanceName> in config.xml (kept by default)")
    ap.add_argument("--api-key", help="API key to write in config.xml, 32 hex digits (default: random); the contract tests expect 0123456789abcdef0123456789abcdef")
    ap.add_argument("--scrub-releases", action="store_true", help="also blank release names and groups")
    args = ap.parse_args()
    app = APPS[args.app]

    args.out.mkdir(parents=True, exist_ok=True)
    if any(args.out.iterdir()):
        sys.exit(f"{args.out} is not empty; refusing to overwrite")

    with tempfile.TemporaryDirectory(prefix="arr-anonymize-") as tmp:
        work = Path(tmp)
        xml_path, db_path = locate_inputs(args.source, work, app)

        new_key = args.api_key or random_api_key()
        if not re.fullmatch(r"[0-9a-f]{32}", new_key):
            sys.exit("--api-key must be 32 lowercase hex digits")
        xml, found = anonymize_config_xml(xml_path.read_text(encoding="utf-8"), new_key, args.instance_name)
        old_key = found.get("ApiKey", "")

        con = sqlite3.connect(db_path)
        con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        con.execute("PRAGMA journal_mode=DELETE")
        level = con.execute("SELECT MAX(Version) FROM VersionInfo").fetchone()[0] if con.execute(
            "SELECT 1 FROM sqlite_master WHERE name='VersionInfo'").fetchone() else None
        anon = Anonymizer(con, args.scrub_releases, app)
        anon.run()
        con.execute("VACUUM")
        integrity = con.execute("PRAGMA integrity_check").fetchone()[0]
        if integrity != "ok":
            sys.exit(f"integrity check failed after anonymisation: {integrity}")
        leaked = anon.verify({old_key: "api key", found.get("SslCertPassword", ""): "ssl password", found.get("PostgresPassword", ""): "postgres password"})
        con.close()
        if leaked:
            print("REFUSING TO WRITE: these original values are still in the database.", file=sys.stderr)
            print("Report the kind and the location lines (not the values) so the scrubber can be fixed:", file=sys.stderr)
            for value, kind, where in leaked[:20]:
                print(f"  [{kind}] {value[:80]!r}", file=sys.stderr)
                for w in where[:6]:
                    print(f"      at {w}", file=sys.stderr)
            if len(leaked) > 20:
                print(f"  ... and {len(leaked) - 20} more", file=sys.stderr)
            sys.exit(1)
        for side in (app["db"] + "-wal", app["db"] + "-shm"):
            if (work / side).exists():
                (work / side).unlink()
        shutil.copy2(db_path, args.out / app["db"])
        (args.out / "config.xml").write_text(xml, encoding="utf-8")

    print(f"wrote {args.out / 'config.xml'} and {args.out / app['db']} (migration level {level})")
    print(f"  API key: {new_key}  (old key removed)")
    print("  web login: unarr / fixture  (original users removed)")
    if "InstanceName" in found and args.instance_name is None:
        print(f"  instance name kept: {found['InstanceName']!r}  (use --instance-name to change)")
    for key in sorted(anon.report):
        print(f"  {anon.report[key]:>6}  {key}")
    print(f"  {len(anon.secrets):>6}  original values checked absent from the output")


if __name__ == "__main__":
    main()
