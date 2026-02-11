# BitNet b1.58 - CPU Inference

## Prerequisites

- CMake >= 3.20
- C++ compiler with AVX2 support
- ~2 GB disk for model + binary

## Build

```bash
./build.sh
```

## Model Download

Download a BitNet b1.58 GGUF model (e.g. 2B4T or 7B) and place it in this directory.

## Usage

The binary is used as a subprocess by sentinel-inference crate.
Direct CLI usage:

```bash
echo "Hello, how are you?" | ./bitnet-inference --model ./model.gguf --threads 8 --max-tokens 256
```
