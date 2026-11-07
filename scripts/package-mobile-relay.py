#!/usr/bin/env python3
"""Package the pinned, patched DSH Relay source without accessing the network."""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import zipfile

UPSTREAM = "april-jk/dsh-relay@709af7262b0ebf80821c99a73f7a9c9fc03819e6"


def main():
    root = Path(__file__).resolve().parents[1]
    resources = root / "packaging/mobile-control"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=root / ".omx/artifacts/mobile-control")
    args = parser.parse_args()
    files = {}
    for line in (resources / "source-files.sha256").read_text(encoding="utf-8").splitlines():
        digest, name = line.split("  ", 1)
        relative = PurePosixPath(name)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError(f"unsafe manifest path: {name}")
        source = args.source / name
        if source.is_symlink() or not source.is_file():
            raise ValueError(f"missing source file: {name}")
        # Every pinned input is UTF-8 text. Git on Windows may check it out as
        # CRLF; distribute the same LF source bytes on every packaging host.
        data = source.read_bytes().replace(b"\r\n", b"\n")
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f"source differs from reviewed version: {name}")
        files[name] = data
    for name in ("compose.pebrel.yml", "Caddyfile.example", "install-relay.sh"):
        files[name] = (resources / name).read_bytes().replace(b"\r\n", b"\n")
    files["PEBREL-DEPLOYMENT.md"] = (
        f"DSH Relay 0.1.8 source: {UPSTREAM}\n\n"
        "Includes Pebrel browser database, reconnect, cold-worker recovery and one-time enrollment patches.\n"
        "The Relay handshake timeout is 130 seconds to allow bounded desktop approval.\n"
        "The encryption library and lockfile retain the pinned source.\n"
        "Source-only: Node 22 and npm dependencies, or Docker, are required.\n"
        "Bind the relay to loopback behind a trusted HTTPS reverse proxy.\n"
        "Set JWT_SECRET and PUBLIC_RELAY_URL in a private .env file.\n"
        "Run `sh install-relay.sh https://your-relay.example` to generate the\n"
        "private configuration, build the container, and verify loopback health.\n"
        "The generated Caddyfile is a template; review it for an existing proxy.\n"
        "Initial phone access requires matching the confirmation code on the computer.\n"
        "The short-lived QR key is replaced over E2EE and cannot enroll another browser.\n"
        "Revoking the computer disables its browser access; existing Agent work continues.\n"
    ).encode("utf-8")
    args.output.mkdir(parents=True, exist_ok=True)
    archive = args.output / "dsh-relay-0.1.8-pebrel-source.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
        for name, data in sorted(files.items()):
            info = zipfile.ZipInfo(name, date_time=(2026, 11, 6, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (0o755 if name == "install-relay.sh" else 0o644) << 16
            bundle.writestr(info, data)
    with zipfile.ZipFile(archive) as bundle:
        if bundle.testzip() is not None or set(bundle.namelist()) != set(files):
            raise ValueError("archive integrity check failed")
        for name, data in files.items():
            if bundle.read(name) != data:
                raise ValueError(f"archive entry differs: {name}")
    metadata = {
        "file": archive.name,
        "bytes": archive.stat().st_size,
        "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "entries": len(files),
        "upstream": UPSTREAM,
        "type": "source-with-deploy-helper",
        "deploy_helper": "install-relay.sh",
        "relay_runtime": "NOT_RUN",
        "network_used": False,
    }
    (args.output / "relay-package.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
