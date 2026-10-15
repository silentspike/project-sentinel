use super::*;

fn legacy<T: Serialize + ?Sized>(domain: &str, value: &T) -> String {
    legacy_json_bytes(domain, &serde_json::to_vec(value).unwrap())
}

fn legacy_json_bytes(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    hex_sha256(&hasher.finalize())
}

fn computations(cache: &ContentDigestCache) -> usize {
    cache
        .computations
        .load(std::sync::atomic::Ordering::Relaxed)
}

#[test]
fn canonical_content_preserves_legacy_bytes_domains_and_struct_field_order() {
    #[derive(Serialize)]
    struct Document<'a> {
        z: &'a str,
        a: [u64; 3],
        negative_zero: f64,
        absent: Option<bool>,
    }
    let value = Document {
        z: "\u{e4}\u{1f600}\n\r\t\"\\\u{0}",
        a: [0, 9_007_199_254_740_993, u64::MAX],
        negative_zero: -0.0,
        absent: None,
    };
    let mut fields = serde_json::Map::new();
    fields.insert("a".into(), serde_json::to_value(value.a).unwrap());
    fields.insert("absent".into(), serde_json::Value::Null);
    fields.insert(
        "negative_zero".into(),
        serde_json::json!(value.negative_zero),
    );
    fields.insert("z".into(), serde_json::json!(value.z));
    let reordered = serde_json::Value::Object(fields);
    assert_ne!(
        serde_json::to_vec(&value).unwrap(),
        serde_json::to_vec(&reordered).unwrap()
    );
    for domain in [
        "",
        "canonical-content-exact",
        "canonical-content-exact\0suffix",
    ] {
        for enabled in [false, true] {
            with_canonical_digest_cache_enabled(enabled, || {
                for _ in 0..2 {
                    assert_eq!(
                        canonical_sha256(domain, &value).unwrap(),
                        legacy(domain, &value)
                    );
                    assert_eq!(
                        canonical_sha256(domain, &reordered).unwrap(),
                        legacy(domain, &reordered)
                    );
                    assert_ne!(
                        canonical_sha256(domain, &value).unwrap(),
                        canonical_sha256(domain, &reordered).unwrap()
                    );
                }
            });
        }
    }
    assert_ne!(
        legacy("canonical-content-exact", &value),
        legacy("canonical-content-exact\0suffix", &value)
    );
}

#[test]
fn canonical_content_serializes_once_at_buffer_and_retention_boundaries() {
    struct Counted<'a> {
        value: &'a str,
        calls: std::cell::Cell<usize>,
    }
    impl Serialize for Counted<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.calls.set(self.calls.get() + 1);
            self.value.serialize(serializer)
        }
    }
    for length in [
        0,
        1,
        DIGEST_BUFFER_BYTES - 1,
        DIGEST_BUFFER_BYTES,
        DIGEST_BUFFER_BYTES + 1,
        CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD - 3,
        CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD - 2,
        CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD - 1,
        CONTENT_DIGEST_MAX_PAYLOAD - 3,
        CONTENT_DIGEST_MAX_PAYLOAD - 2,
        CONTENT_DIGEST_MAX_PAYLOAD - 1,
        CONTENT_DIGEST_MAX_PAYLOAD + 17,
    ] {
        let value = "x".repeat(length);
        assert_eq!(serde_json::to_vec(&value).unwrap().len(), length + 2);
        let expected = legacy("canonical-content-boundary", &value);
        let counted = Counted {
            value: &value,
            calls: std::cell::Cell::new(0),
        };
        for enabled in [true, true, false] {
            let before = counted.calls.get();
            assert_eq!(
                with_canonical_digest_cache_enabled(enabled, || {
                    canonical_sha256("canonical-content-boundary", &counted).unwrap()
                }),
                expected
            );
            assert_eq!(
                counted.calls.get(),
                before + 1,
                "cache hits and oversized fallback must each serialize exactly once"
            );
        }
    }
}

#[test]
fn canonical_content_rechecks_one_changed_byte_at_the_same_address() {
    let mut value = vec![b'a'; 128];
    let pointer = value.as_ptr();
    with_canonical_digest_cache_enabled(true, || {
        let original = canonical_sha256("canonical-content-mutated", &value).unwrap();
        assert_eq!(original, legacy("canonical-content-mutated", &value));
        value[37] ^= 1;
        assert_eq!(value.as_ptr(), pointer);
        let changed = canonical_sha256("canonical-content-mutated", &value).unwrap();
        assert_eq!(changed, legacy("canonical-content-mutated", &value));
        assert_ne!(changed, original);
        value[37] ^= 1;
        assert_eq!(
            canonical_sha256("canonical-content-mutated", &value).unwrap(),
            original
        );
    });
}

#[test]
fn canonical_content_serializer_failure_cannot_reuse_even_a_complete_cached_json_value() {
    struct Failing<'a> {
        value: &'a str,
        complete: bool,
        calls: std::cell::Cell<usize>,
    }
    impl Serialize for Failing<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            self.calls.set(self.calls.get() + 1);
            let mut sequence = serializer.serialize_seq(Some(1))?;
            sequence.serialize_element(self.value)?;
            if self.complete {
                let _ = sequence.end()?;
            }
            Err(serde::ser::Error::custom(
                "injected canonical content serialization failure",
            ))
        }
    }
    for length in [8, CONTENT_DIGEST_MAX_PAYLOAD + 17] {
        let value = "q".repeat(length);
        let valid = vec![value.as_str()];
        for enabled in [false, true] {
            with_canonical_digest_cache_enabled(enabled, || {
                let expected = legacy("canonical-content-failure", &valid);
                assert_eq!(
                    canonical_sha256("canonical-content-failure", &valid).unwrap(),
                    expected
                );
                for complete in [false, true] {
                    let failing = Failing {
                        value: &value,
                        complete,
                        calls: std::cell::Cell::new(0),
                    };
                    for _ in 0..2 {
                        let before = failing.calls.get();
                        assert_eq!(
                            canonical_sha256("canonical-content-failure", &failing)
                                .unwrap_err()
                                .code,
                            WorkflowErrorCode::InvalidInput
                        );
                        assert_eq!(failing.calls.get(), before + 1);
                    }
                }
                assert_eq!(
                    canonical_sha256("canonical-content-failure", &valid).unwrap(),
                    expected
                );
            });
        }
    }
}

#[test]
fn canonical_content_reuses_exact_serialized_bytes_from_distinct_allocations() {
    let cache = ContentDigestCache::new();
    let first = "independent-owned-canonical-content".to_owned();
    let second = first.clone();
    assert_ne!(first.as_ptr(), second.as_ptr());
    with_canonical_digest_cache_enabled(true, || {
        let expected = legacy("canonical-content-owned", &first);
        assert_eq!(
            canonical_sha256_with_cache("canonical-content-owned", &first, &cache).unwrap(),
            expected
        );
        assert_eq!(computations(&cache), 1);
        assert_eq!(
            canonical_sha256_with_cache("canonical-content-owned", &second, &cache).unwrap(),
            expected
        );
        assert_eq!(
            computations(&cache),
            1,
            "equal JSON bytes should avoid a second SHA computation"
        );
    });
}

#[test]
fn canonical_content_collisions_compare_encoding_domain_length_and_every_byte() {
    let cache = ContentDigestCache::new();
    let first = b"[0,1,255]";
    let changed = b"[0,2,255]";
    let shorter = b"[0,1,25]";
    let cases = [
        (
            DigestEncoding::CanonicalJson,
            "canonical-content-collision",
            first.as_slice(),
        ),
        (
            DigestEncoding::NumericByteArray,
            "canonical-content-collision",
            first.as_slice(),
        ),
        (
            DigestEncoding::CanonicalJson,
            "canonical-content-other-domain",
            first.as_slice(),
        ),
        (
            DigestEncoding::CanonicalJson,
            "canonical-content-collision",
            changed.as_slice(),
        ),
        (
            DigestEncoding::CanonicalJson,
            "canonical-content-collision",
            shorter.as_slice(),
        ),
    ];
    for (index, (encoding, domain, bytes)) in cases.iter().enumerate() {
        let expected = match encoding {
            DigestEncoding::CanonicalJson => legacy_json_bytes(domain, bytes),
            DigestEncoding::NumericByteArray => legacy(domain, bytes),
        };
        let (digest, hit) = cache.digest_encoded_at(0, *encoding, domain, bytes);
        assert_eq!(digest, expected);
        assert!(!hit, "a fingerprint collision is not content equality");
        assert_eq!(computations(&cache), index + 1);
    }
    assert_ne!(
        cache
            .digest_encoded_at(
                0,
                DigestEncoding::CanonicalJson,
                "canonical-content-collision",
                first
            )
            .0,
        cache
            .digest_encoded_at(
                0,
                DigestEncoding::NumericByteArray,
                "canonical-content-collision",
                first
            )
            .0
    );
    for (encoding, domain, bytes) in cases {
        let expected = match encoding {
            DigestEncoding::CanonicalJson => legacy_json_bytes(domain, bytes),
            DigestEncoding::NumericByteArray => legacy(domain, bytes),
        };
        let (digest, hit) = cache.digest_encoded_at(0, encoding, domain, bytes);
        assert_eq!(digest, expected);
        assert!(hit);
    }
    assert_eq!(computations(&cache), cases.len());
}

#[test]
fn canonical_content_and_numeric_arrays_share_existing_eviction_byte_and_entry_budgets() {
    const CANONICAL_BYTES: usize = CONTENT_DIGEST_CANONICAL_SHARD_BYTES;
    const CANONICAL_ENTRIES: usize = CONTENT_DIGEST_CANONICAL_SHARD_ENTRIES;
    assert_eq!(CANONICAL_BYTES, 128 * 1024);
    assert_eq!(CANONICAL_ENTRIES, 32);
    assert_eq!(
        CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD + ContentDigestEntry::payload_cost(0),
        CANONICAL_BYTES
    );
    assert_eq!(
        CONTENT_DIGEST_SHARDS * CONTENT_DIGEST_SHARD_BYTES,
        32 * 1024 * 1024
    );
    let cache = ContentDigestCache::new();
    let numeric_len =
        (CONTENT_DIGEST_SHARD_BYTES - CANONICAL_BYTES) / 4 - ContentDigestEntry::payload_cost(0);
    let mut numeric = vec![0; numeric_len];
    let mut payload = serde_json::to_vec(
        &"x".repeat(CANONICAL_BYTES / 2 - ContentDigestEntry::payload_cost(0) - 2),
    )
    .unwrap();
    for shard_index in 0..CONTENT_DIGEST_SHARDS {
        for generation in 0_u8..4 {
            numeric[0] = generation;
            let _ = cache.digest_at(
                shard_index as u64,
                "canonical-content-byte-budget",
                &numeric,
            );
        }
        for generation in 0_u8..6 {
            payload[1] = b'a' + generation;
            let (_, hit) = cache.digest_encoded_at(
                shard_index as u64,
                DigestEncoding::CanonicalJson,
                "canonical-content-byte-budget",
                &payload,
            );
            assert!(!hit);
            let shard = cache.shards[shard_index].lock().unwrap();
            assert!(shard.bytes <= CONTENT_DIGEST_SHARD_BYTES);
            assert!(shard.entries.len() <= CONTENT_DIGEST_SHARD_ENTRIES);
            let canonical = shard
                .entries
                .iter()
                .filter(|entry| entry.encoding == DigestEncoding::CanonicalJson);
            assert!(canonical.clone().count() <= CANONICAL_ENTRIES);
            assert!(canonical.map(ContentDigestEntry::cost).sum::<usize>() <= CANONICAL_BYTES);
            assert_eq!(
                shard.bytes,
                shard
                    .entries
                    .iter()
                    .map(ContentDigestEntry::cost)
                    .sum::<usize>()
            );
            assert!(shard
                .entries
                .iter()
                .all(|entry| entry.bytes.len() <= CONTENT_DIGEST_MAX_PAYLOAD));
        }
        assert!(cache.shards[shard_index]
            .lock()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.encoding == DigestEncoding::CanonicalJson));
        // Numeric admission under shared byte pressure must evict canonical first.
        let _ = cache.digest_at(shard_index as u64, "canonical-content-byte-budget", b"new");
        for generation in 0_u8..4 {
            numeric[0] = generation;
            let before = computations(&cache);
            let (_, hit) = cache.digest_encoded_at(
                shard_index as u64,
                DigestEncoding::NumericByteArray,
                "canonical-content-byte-budget",
                &numeric,
            );
            assert!(
                hit,
                "canonical byte flooding must preserve the working numeric inventory"
            );
            assert_eq!(computations(&cache), before);
        }
    }
    let retained = cache
        .shards
        .iter()
        .map(|shard| shard.lock().unwrap().bytes)
        .sum::<usize>();
    assert!(retained <= 32 * 1024 * 1024);
    payload[1] = b'a';
    let before = computations(&cache);
    let (digest, hit) = cache.digest_encoded_at(
        0,
        DigestEncoding::CanonicalJson,
        "canonical-content-byte-budget",
        &payload,
    );
    assert!(
        !hit,
        "oldest canonical content must be evicted under its own byte quota"
    );
    assert_eq!(
        digest,
        legacy_json_bytes("canonical-content-byte-budget", &payload)
    );
    assert_eq!(computations(&cache), before + 1);

    let small = ContentDigestCache::new();
    for index in 0..CONTENT_DIGEST_SHARD_ENTRIES - CANONICAL_ENTRIES {
        let _ = small.digest_at(0, "canonical-content-entry-budget", &index.to_le_bytes());
    }
    for index in 0..CANONICAL_ENTRIES * 2 {
        let payload = serde_json::to_vec(&index).unwrap();
        let _ = small.digest_encoded_at(
            0,
            DigestEncoding::CanonicalJson,
            "canonical-content-entry-budget",
            &payload,
        );
        let shard = small.shards[0].lock().unwrap();
        assert!(shard.entries.len() <= CONTENT_DIGEST_SHARD_ENTRIES);
        assert!(shard.bytes <= CONTENT_DIGEST_SHARD_BYTES);
        let canonical = shard
            .entries
            .iter()
            .filter(|entry| entry.encoding == DigestEncoding::CanonicalJson);
        assert!(canonical.clone().count() <= CANONICAL_ENTRIES);
        assert!(canonical.map(ContentDigestEntry::cost).sum::<usize>() <= CANONICAL_BYTES);
    }
    let before = computations(&small);
    assert!(
        !small
            .digest_encoded_at(
                0,
                DigestEncoding::CanonicalJson,
                "canonical-content-entry-budget",
                b"0"
            )
            .1
    );
    assert_eq!(computations(&small), before + 1);
    // Fill the remaining shared slots with numeric entries, then flood canonical
    // again. Canonical admission must not displace numeric-only full shards.
    for index in CONTENT_DIGEST_SHARD_ENTRIES - CANONICAL_ENTRIES..CONTENT_DIGEST_SHARD_ENTRIES {
        let _ = small.digest_at(0, "canonical-content-entry-budget", &index.to_le_bytes());
    }
    assert_eq!(
        small.shards[0].lock().unwrap().entries.len(),
        CONTENT_DIGEST_SHARD_ENTRIES
    );
    for index in 0..CANONICAL_ENTRIES * 2 {
        let _ = small.digest_encoded_at(
            0,
            DigestEncoding::CanonicalJson,
            "canonical-content-entry-budget",
            &serde_json::to_vec(&index).unwrap(),
        );
    }
    let before = computations(&small);
    for index in 0..CONTENT_DIGEST_SHARD_ENTRIES {
        assert!(
            small
                .digest_encoded_at(
                    0,
                    DigestEncoding::NumericByteArray,
                    "canonical-content-entry-budget",
                    &index.to_le_bytes()
                )
                .1,
            "canonical entry flooding or numeric refill evicted a working numeric entry"
        );
    }
    assert_eq!(computations(&small), before);
    let shard = small.shards[0].lock().unwrap();
    assert_eq!(shard.entries.len(), CONTENT_DIGEST_SHARD_ENTRIES);
    assert!(shard.bytes <= CONTENT_DIGEST_SHARD_BYTES);
    assert!(shard
        .entries
        .iter()
        .all(|entry| entry.encoding == DigestEncoding::NumericByteArray));
}

#[test]
fn canonical_content_oversize_writer_falls_back_once_without_retaining_or_growing_past_limit() {
    let cache = ContentDigestCache::new();
    let bytes = serde_json::to_vec(&"s".repeat(CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD + 17)).unwrap();
    let expected = legacy_json_bytes("canonical-content-streamed", &bytes);
    for chunk_size in [
        DIGEST_BUFFER_BYTES - 1,
        DIGEST_BUFFER_BYTES,
        DIGEST_BUFFER_BYTES + 1,
        CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD,
        CONTENT_DIGEST_MAX_PAYLOAD,
        bytes.len(),
    ] {
        let mut writer = CanonicalDigestWriter::new("canonical-content-streamed");
        for chunk in bytes.chunks(chunk_size) {
            writer.write_all(chunk).unwrap();
            writer.flush().unwrap();
            writer.flush().unwrap();
            assert!(writer.buffered.len() <= CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD);
            assert!(writer.buffered.capacity() <= CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD);
            if writer.streaming.is_some() {
                assert!(writer.buffered.is_empty());
                assert_eq!(writer.buffered.capacity(), 0);
            }
        }
        assert!(writer.streaming.is_some());
        assert_eq!(writer.finish(&cache), expected);
    }
    assert_eq!(
        computations(&cache),
        0,
        "oversized streams must bypass cache admission entirely"
    );
    assert!(cache
        .shards
        .iter()
        .all(|shard| shard.lock().unwrap().entries.is_empty()));
    for _ in 0..2 {
        let (digest, hit) = cache.digest_encoded_at(
            0,
            DigestEncoding::CanonicalJson,
            "canonical-content-streamed",
            &bytes,
        );
        assert_eq!(digest, expected);
        assert!(!hit);
    }
    assert_eq!(computations(&cache), 2);
    assert!(cache
        .shards
        .iter()
        .all(|shard| shard.lock().unwrap().entries.is_empty()));
}

#[test]
fn canonical_content_poisoned_cache_recomputes_instead_of_trusting_retained_digests() {
    let cache = ContentDigestCache::new();
    let bytes = b"[0,1,255]";
    for encoding in [
        DigestEncoding::CanonicalJson,
        DigestEncoding::NumericByteArray,
    ] {
        assert!(
            !cache
                .digest_encoded_at(0, encoding, "canonical-content-poison", bytes)
                .1
        );
    }
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut shard = cache.shards[0].lock().unwrap();
        for entry in &mut shard.entries {
            entry.digest = "0".repeat(64);
        }
        panic!("controlled content-cache poison");
    }));
    assert!(poisoned.is_err());
    assert!(cache.shards[0].is_poisoned());
    for _ in 0..2 {
        for encoding in [
            DigestEncoding::CanonicalJson,
            DigestEncoding::NumericByteArray,
        ] {
            let expected = match encoding {
                DigestEncoding::CanonicalJson => {
                    legacy_json_bytes("canonical-content-poison", bytes)
                }
                DigestEncoding::NumericByteArray => legacy("canonical-content-poison", bytes),
            };
            let before = computations(&cache);
            let (digest, hit) =
                cache.digest_encoded_at(0, encoding, "canonical-content-poison", bytes);
            assert_eq!(digest, expected);
            assert!(!hit);
            assert_eq!(computations(&cache), before + 1);
        }
    }
}

#[test]
fn canonical_content_cache_mode_nesting_and_serializer_unwind_restore_the_previous_mode() {
    struct Panics;
    impl Serialize for Panics {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let _ = serializer.serialize_str("complete-before-unwind")?;
            panic!("controlled canonical serializer unwind");
        }
    }
    let original = CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get());
    for baseline in [false, true] {
        let cache = ContentDigestCache::new();
        with_canonical_digest_cache_enabled(baseline, || {
            assert_eq!(
                CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get()),
                baseline
            );
            with_canonical_digest_cache_enabled(!baseline, || {
                assert_eq!(
                    CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get()),
                    !baseline
                );
                with_canonical_digest_cache_enabled(baseline, || {
                    assert_eq!(
                        CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get()),
                        baseline
                    );
                });
                assert_eq!(
                    CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get()),
                    !baseline
                );
            });
            assert_eq!(
                CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get()),
                baseline
            );
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_canonical_digest_cache_enabled(!baseline, || {
                    canonical_sha256_with_cache("canonical-content-unwind", &Panics, &cache)
                })
            }));
            assert!(unwind.is_err());
            assert_eq!(
                CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get()),
                baseline
            );
            assert_eq!(
                computations(&cache),
                0,
                "unwound serializers cannot publish content"
            );
            assert!(cache
                .shards
                .iter()
                .all(|shard| shard.lock().unwrap().entries.is_empty()));
        });
        assert_eq!(
            CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get()),
            original
        );
        with_canonical_digest_cache_enabled(true, || {
            let expected = legacy("canonical-content-unwind", &"complete-before-unwind");
            for _ in 0..2 {
                assert_eq!(
                    canonical_sha256_with_cache(
                        "canonical-content-unwind",
                        &"complete-before-unwind",
                        &cache
                    )
                    .unwrap(),
                    expected
                );
            }
            assert_eq!(computations(&cache), 1);
        });
    }
    assert_eq!(
        CANONICAL_DIGEST_CACHE_ENABLED.with(|mode| mode.get()),
        original
    );
}
