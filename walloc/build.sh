#!/bin/bash
# SUBMODULE_001: LAYER-W/ENGINE/walloc.rs BUILD SCRIPT

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

echo -e "Building for WebAssembly...\n"
cargo build --release --target=wasm32-unknown-unknown

echo -e "Do you want to use wasm-bindgen as the build tool? Otherwise, wasm-pack will be used. [Y/n]: "
read -r use_bindgen
use_bindgen=${use_bindgen:-Y}

if [[ "$use_bindgen" =~ ^[Yy]$ ]]; then
    echo -e "Using wasm-bindgen for the build...\n"
    
    wasm-bindgen target/wasm32-unknown-unknown/release/walloc.wasm --out-dir ./wbg --target web

    echo -e "Using wasm-opt on wasm-bindgen build..."
    wasm-opt -Oz ./wbg/walloc_bg.wasm -o ./wbg/walloc_bg.wasm

    echo -e "Done Building. Copying the Wasm Bindgen build to test-runner/wbg directory...\n"
    if [ ! -d "../test-runner/wbg" ]; then
        mkdir -p ../test-runner/wbg
    fi
    cp -r wbg/* ../test-runner/wbg
    rm -rf wbg
    echo -e "See test-runner/index.html via server for the Wasm Bindgen build. It uses the lib.rs\n"
else
    echo -e "Skipping wasm-bindgen. Using wasm-pack instead.\n"
    wasm-pack build --target web --out-dir wasm
    echo -e "Copying the Wasm Pack build to test-runner/wmpkg directory...\n"
    if [ ! -d "../test-runner/wmpkg" ]; then
        mkdir -p ../test-runner/wmpkg
    fi
    cp -r wasm/* ../test-runner/wmpkg
    rm -rf wasm
    echo -e "See test-runner/index.html via server for the Wasm Pack build. It uses the lib.rs\n"
fi


echo -e "Build process completed!\n"