#!/bin/bash
set -euo pipefail
# BitNet b1.58 build script for i5-1235U (AVX2)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

if [ ! -d "bitnet-src" ]; then
    git clone https://github.com/microsoft/BitNet.git bitnet-src
fi

cd bitnet-src
mkdir -p build && cd build
cmake .. -DCMAKE_BUILD_TYPE=Release -DBITNET_AVX2=ON -DBITNET_THREADS=8
make -j"$(nproc)"
cp bin/bitnet-inference "$SCRIPT_DIR/bitnet-inference"
echo "BitNet built successfully: $SCRIPT_DIR/bitnet-inference"
