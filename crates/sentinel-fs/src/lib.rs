//! sentinel-fs: CAS-FUSE agent filesystem with SHA-256 dedup and zstd compression.
//!
//! Provides isolated, deduplicated filesystems for agents via a single FUSE mount.
//! Architecture: CAS Store (blobs) + redb Metadata + Layer Manager (base/agent CoW) + FUSE.

pub mod cas;
pub mod cli;
pub mod fuse;
pub mod layer;
pub mod metadata;
