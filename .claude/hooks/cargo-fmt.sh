#!/bin/bash
# Auto-format Rust files after edit/write.
cargo fmt --all 2>/dev/null
exit 0
