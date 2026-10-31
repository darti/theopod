"""Prepare pinned disposable feasibility probes. Never runs probes or accesses a board."""
import argparse
import hashlib
import pathlib
import shutil
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("mode", choices=["host", "linked", "cdn"])
parser.add_argument("output", type=pathlib.Path)
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parent
repo = here.parents[2]
out = args.output.resolve()
out.mkdir()  # Refuse overwrite, including a partial earlier experiment.
for name, expected in {
    "gtsr4.der": "349dfa4058c5e263123b398ae795573c4e1313c83fe68f93556cd5e8031b3c7d",
    "isrgrootx1.der": "96bcec06264976f37460779acf28c5a7cfe8a3c0aae11a8ffcee05c0bddf08c6",
}.items():
    if hashlib.sha256((here / name).read_bytes()).hexdigest() != expected:
        raise SystemExit("Unexpected trust-anchor bytes: " + name)

if args.mode in {"host", "linked"}:
    base = out / "base"
    base.mkdir()
    archive = out / "base.tar"
    with archive.open("wb") as stream:
        subprocess.run(["git", "archive", "acca56f"], cwd=repo, stdout=stream, check=True)
    subprocess.run(["tar", "-xf", str(archive), "-C", str(base)], check=True)
    archive.unlink()

if args.mode == "linked":
    shutil.copy(here / "linked-Cargo.toml", base / "Cargo.toml")
    shutil.copy(here / "linked-Cargo.lock", base / "Cargo.lock")
    for source, dest in {
        "linked-probe.rs": "t106_probe.rs",
        "linked-transport.rs": "t106_transport.rs",
        "linked-regions.rs": "t106_regions.rs",
    }.items():
        shutil.copy(here / source, base / "src/device" / dest)
    shutil.copy(here / "gtsr4.der", base / "gtsr4.der")
    subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-i", str(here / "linked-device.patch")], cwd=base, check=True)
    print(base)
else:
    probe = out / "probe"
    (probe / "src").mkdir(parents=True)
    manifest = (here / f"{args.mode}-Cargo.toml").read_text()
    if args.mode == "host":
        manifest = manifest.replace("{{BASE}}", str(base).replace("\\", "\\\\").replace('"', '\\"'))
        for source, dest in {"host-lib.rs": "lib.rs", "regions.rs": "regions.rs", "regions_tests.rs": "regions_tests.rs"}.items():
            shutil.copy(here / source, probe / "src" / dest)
    else:
        for source, dest in {"cdn-main.rs": "main.rs", "cdn-lib.rs": "lib.rs"}.items():
            shutil.copy(here / source, probe / "src" / dest)
        for name in ["gtsr4.der", "isrgrootx1.der", "audio.path"]:
            shutil.copy(here / name, probe / name)
    (probe / "Cargo.toml").write_text(manifest)
    shutil.copy(here / f"{args.mode}-Cargo.lock", probe / "Cargo.lock")
    print(probe)
