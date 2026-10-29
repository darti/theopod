"""Prepare an isolated EKV recovery experiment; no firmware dependency changes."""
import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("ekv_source", type=pathlib.Path)
parser.add_argument("output", type=pathlib.Path)
parser.add_argument("--unpatched", action="store_true", help="Retain both original EKV defects")
args = parser.parse_args()
source, output = args.ekv_source.resolve(), args.output.resolve()
here = pathlib.Path(__file__).resolve().parent
original = source / "src/file.rs"
expected = "d1490d0723b09643f83725ece5001231fb62f1344857727743690233649f32e4"
if hashlib.sha256(original.read_bytes()).hexdigest() != expected:
    raise SystemExit("Expected the unmodified registry EKV 1.0.0 src/file.rs")
output.mkdir()  # Never overwrite another experiment.
ekv = output / "ekv"
shutil.copytree(source, ekv)
if not args.unpatched:
    subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-i",
                    str(here / "ekv-metadata-publication.patch")], cwd=ekv, check=True)
db = output / "db"
subprocess.run([sys.executable, str(here.parent / "t106/prepare.py"), "db", str(db)], check=True)
for name in ("rollover_tests.rs", "history_tests.rs"):
    shutil.copy(here / name, db / "src" / name)
manifest = db / "Cargo.toml"
# TOML accepts double-quoted strings with JSON's escaping for these local paths.
with manifest.open("a") as stream:
    stream.write("\n[patch.crates-io]\nekv = { path = " + json.dumps(str(ekv)) + " }\n")
print("Set THEO_T106_DIR=" + str(db))
