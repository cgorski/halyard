#!/bin/sh
export HALYARD_OUTPUT_NAME="service_mode"
cargo halyard --manifest-path halyard/tests/service_mode/Cargo.toml build
