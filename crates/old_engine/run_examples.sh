#!/usr/bin/env bash
set -euo pipefail

# Detect all crates (directories containing Cargo.toml)
CRATES=$(find . -name "Cargo.toml" -not -path "*/target/*")

# Whether to use --release
RELEASE_FLAG=${1:-}
if [[ "$RELEASE_FLAG" == "--release" ]]; then
  BUILD_MODE="--release"
  echo " Running all examples in RELEASE mode"
else
  BUILD_MODE=""
  echo " Running all examples in DEBUG mode"
fi

echo

for crate_toml in $CRATES; do
  crate_dir=$(dirname "$crate_toml")
  crate_name=$(basename "$crate_dir")

  examples_dir="$crate_dir/examples"
  if [[ -d "$examples_dir" ]]; then
    echo "Crate: $crate_name"
    echo "----------------------------------------"

    for example in "$examples_dir"/*.rs; do
      [[ -e "$example" ]] || continue # skip empty dirs
      example_name=$(basename "$example" .rs)

      echo "Running example: $crate_name/$example_name"
      (
        cd "$crate_dir"
        cargo run $BUILD_MODE --example "$example_name"
      )
      echo "Finished: $crate_name/$example_name"
      echo
    done
  fi
done

echo "All examples completed successfully!"
