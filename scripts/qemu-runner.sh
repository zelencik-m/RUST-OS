#!/usr/bin/env bash
# Cargo runner wrapper around `bootimage runner`.
#
# The ext2 driver writes to its disk, so QEMU must never be pointed at the
# committed ext2.img. This script stages a working copy in target/ and the
# bootimage args in Cargo.toml point QEMU at that copy:
#
#   - test kernels (built under .../deps/) get a fresh copy on every run, so
#     each suite starts from the same disk state;
#   - `cargo run` keeps its copy between runs so files you create persist.
#     Delete target/ext2-disk.img (or `cargo clean`) to reset it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_img="$repo_root/ext2.img"
disk_img="$repo_root/target/ext2-disk.img"

kernel="${1:?usage: qemu-runner.sh <kernel-binary> [args...]}"

mkdir -p "$repo_root/target"
if [[ "$kernel" == */deps/* || ! -f "$disk_img" ]]; then
    cp "$source_img" "$disk_img"
fi

exec bootimage runner "$@"
