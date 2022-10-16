#!/bin/bash
RUSTFLAGS="-L $(brew --prefix)/lib" cargo build --release