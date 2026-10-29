"""Prepare disposable t-106 probes; never build or mutate the firmware."""
import pathlib
import shutil
import sys

mode, output = sys.argv[1:]
if mode not in {"db", "https", "xtensa"}:
    raise SystemExit("mode must be db, https or xtensa")
here = pathlib.Path(__file__).resolve().parent
repo = here.parents[2]
dest = pathlib.Path(output).resolve()
dest.mkdir()  # Refuse to overwrite an existing experiment.
(dest / "src").mkdir()
if mode == "db":
    manifest = '''[package]
name = "theo-t106-probe"
version = "0.0.0"
edition = "2024"
[dependencies]
allocator-api2 = { version = "0.3", default-features = false, features = ["alloc"] }
ekv = { version = "=1.0.0", features = ["crc", "max-page-count-4096", "max-value-size-4096"] }
futures = { version = "0.3", features = ["executor"] }
embassy-sync-06 = { package = "embassy-sync", version = "0.6" }
'''
    source = f'''extern crate alloc;
#[path = "{repo}/crates/theo-storage/src/page_store.rs"]
pub mod page_store;
pub mod adapter;
#[cfg(test)] mod raw_file_tests;
#[cfg(test)] mod rollover_tests;
'''
    for name in ("adapter.rs", "raw_file_tests.rs", "rollover_tests.rs"):
        shutil.copy(here / name, dest / "src" / name)
    old_name = "theo-t106-db"
else:
    manifest = (here / f"{mode}-Cargo.toml").read_text()
    source = (here / ("https.rs" if mode == "https" else "xtensa.rs")).read_text()
    old_name = "theo-t106-https-host" if mode == "https" else "theo-t106-net"
    if mode == "https":
        shutil.copy(here / "https_host.rs", dest / "src" / "main.rs")
    else:
        shutil.copy(here / "adapter.rs", dest / "src" / "adapter.rs")
        page = (repo / "crates/theo-storage/src/page_store.rs").read_text()
        page += '\n#[unsafe(no_mangle)] pub static T106_VERSION_BYTES: [u8; core::mem::size_of::<Option<Version>>()] = [0; core::mem::size_of::<Option<Version>>()];\n'
        (dest / "src" / "page_store.rs").write_text(page)
(dest / "src" / "lib.rs").write_text(source)
(dest / "Cargo.toml").write_text(manifest.replace(old_name, "theo-t106-probe"))
lock = (here / f"{mode}-Cargo.lock").read_text()
(dest / "Cargo.lock").write_text(lock.replace(old_name, "theo-t106-probe"))
print(dest)
