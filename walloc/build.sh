#!/bin/bash
# LAYER-W/ENGINE/walloc.rs BUILD SCRIPT.

set -e  # Exit on error

confirm() {
    local message="$1"
    while true; do
        echo -n "Proceed with $message? [Y/n]: "
        read -r confirm
        case "$confirm" in
            [Yy]) return 0 ;;  # Proceed
            [Nn]) echo "Cancelled."; exit 0 ;;  # Exit
            *) echo "Invalid input. Please enter y or n." ;;
        esac
    done
}

echo -e "\n===== BUILD: SUBMODULE_001: LAYER-W/ENGINE/walloc.rs =====\n"
echo -e "  1) Wasm32 Unknown (wasm32-unknown-unknown)"
echo -e "  2) Native (cargo, rustc, rustup defaults)"
read -p "Selection [1/2]: " choice
choice=${choice:-1}

build_wasm32() {
    echo -e "Building for WebAssembly...\n"
    cargo build --release --target=wasm32-unknown-unknown

    echo -e "Using wasm-bindgen for the build...\n"

    wasm-bindgen target/wasm32-unknown-unknown/release/walloc.wasm --out-dir ./wbg --target web

    echo -e "Done Building. Copying the Wasm Bindgen build to test-runner/wbg directory...\n"
    if [ ! -d "../test-runner/wbg" ]; then
        mkdir -p ../test-runner/wbg
    fi
    cp -r wbg/* ../test-runner/wbg
    rm -rf wbg
    echo -e "See test-runner/index.html via server for the Wasm Bindgen build. It uses the lib.rs\n"
}

build_native() {
    echo -e "Building for native platform...\n"
    cargo build --release
    echo -e "Done Building. Running the native binary...\n"
    cargo run --release
}

case $choice in
1)
    build_wasm32
    ;;
2)
    build_native
    ;;
*)
    echo -e "Invalid choice. Exiting.\n"
    exit 1
    ;;
esac