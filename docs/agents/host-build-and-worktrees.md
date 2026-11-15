# Host builds and worktrees

Read before any host build or when working from a worktree.

- `.cargo/config.toml` forces the xtensa target: host builds need
  `RUSTUP_TOOLCHAIN=stable cargo <cmd> --target $(rustc +stable -vV | sed -n 's/^host: //p')`.
- In a worktree under `.claude/worktrees/`, Cargo also reads the main
  checkout's `.cargo/config.toml` and links `-Tlinkall.x` twice
  ("region 'vectors_seg' already defined"): prefix link/flash recipes with
  `RUSTFLAGS="-C link-arg=-Tlinkall.x"`.
- Slint renderer behaviour (rotation, dirty regions) can be checked on the
  host with a scratch `std` crate using the same `slint` version and
  `renderer-software`, rendering into a buffer.
