//! sentinel-fs: CAS-FUSE agent filesystem with SHA-256 dedup and zstd compression.
//!
//! Provides isolated, deduplicated filesystems for agents via a single FUSE mount.
//! Architecture: CAS Store (blobs) + redb Metadata + Layer Manager (base/agent CoW) + FUSE.
//!
//! Artifact Plane (Issue #56): content-defined chunking, transactional ingest,
//! streaming read planner, and refcount GC for multi-chunk objects.

pub mod artifact;
pub mod cas;
pub mod chunker;
pub mod cli;
pub mod fuse;
pub mod gc;
pub mod ingest;
pub mod layer;
pub mod metadata;
pub mod read_planner;
