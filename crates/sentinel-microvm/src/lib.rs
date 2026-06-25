//! sentinel-microvm: Firecracker-microVM-Adapter fuer den Nano-Container-Vertrag (#417).
//!
//! Vierte `NanoRuntime`-Familie neben ECS-native, WASM/Wasmtime und bwrap+Landlock (DEV-007).
//! microVM = maximale Isolation / Escape-Hatch: jeder Workload laeuft als hardware-virtualisierte
//! Firecracker-microVM ueber KVM, mit eigenem Gast-Kernel und rootfs.
//!
//! ## Snapshot-Semantik
//!
//! Die `NanoSnapshot.payload` traegt STABILE Metadaten (Config + deterministische Pfade zu den
//! Firecracker mem/state-Dateien je `workload_id`), NICHT die volatilen Guest-RAM-Bytes. Der echte
//! Speicher-Snapshot wird via Firecracker-Snapshot-API erzeugt und liegt in den referenzierten
//! Dateien. Diese Trennung macht `restore(snapshot(x))` payload-stabil (Conformance-Vertrag #408),
//! waehrend der eigentliche RAM-Zustand ueber die Firecracker-Dateien transportiert wird.
//!
//! Die mem/state-Dateien sind gewoehnliche Disk-Dateien und damit content-addressierbar
//! (siehe [`manifest`], #500a, `docs/microvm-ram-boundary.md`): sie reisen als `BlockRef` statt
//! als Inline-Kopie. Ein Multi-GB-RAM-Dump dedupliziert aber praktisch nicht und wird daher als
//! ein SHA-256-Whole-Blob referenziert. Die lebenden Guest-RAM-Pages werden hier NICHT erfasst —
//! tiefe microVM-Migration (Post-Copy, Consistency-Class) ist Track F (#554), nicht #500a.
//!
//! ## Voraussetzungen (Minimal-Setup)
//!
//! - KVM: `/dev/kvm` muss vorhanden und beschreibbar sein (sonst sauberer Fehler bei `spawn`).
//! - Firecracker-Binary (Pfad via [`MicrovmConfig`], Default `firecracker` im PATH).
//! - Gast-Kernel-Image (vmlinux) und rootfs (ext4) — Pfade via [`MicrovmConfig`].
//!
//! Greenfield-PoC (prio:low): keine produktive Kernel-/rootfs-Image-Pipeline; cross-node-Migration
//! ist out-of-scope (Multi-Node-gated).

mod firecracker;
pub mod manifest;
mod microvm;

pub use microvm::{MicrovmConfig, MicrovmNanoRuntime};
pub use sentinel_common::nano_runtime::RUNTIME_MICROVM;
