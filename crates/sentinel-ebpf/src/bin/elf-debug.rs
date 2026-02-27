//! Minimal ELF parse debug tool.
//! Tests each layer of parsing to find the exact failure point.

#[cfg(feature = "ebpf")]
fn main() {
    let probe_bytes = include_bytes!("../../probes/agent-health.o");

    println!("Probe size: {} bytes", probe_bytes.len());
    println!(
        "Magic: {:02x} {:02x} {:02x} {:02x}",
        probe_bytes[0], probe_bytes[1], probe_bytes[2], probe_bytes[3]
    );
    println!("Pointer address: {:p}", probe_bytes.as_ptr());
    println!(
        "Pointer alignment: {} (mod 8 = {})",
        probe_bytes.as_ptr() as usize,
        probe_bytes.as_ptr() as usize % 8
    );

    // Test with original (potentially unaligned) data
    println!("\n=== Test 1: Direct include_bytes (possibly unaligned) ===");
    match object::File::parse(&probe_bytes[..]) {
        Ok(_) => println!("  object::File::parse: OK"),
        Err(e) => println!("  object::File::parse: FAILED: {}", e),
    }

    // Test with aligned copy
    println!("\n=== Test 2: Aligned copy (8-byte aligned) ===");
    let mut aligned = vec![0u8; probe_bytes.len()];
    // Ensure alignment by using a Vec (which is heap-allocated and typically aligned)
    aligned.copy_from_slice(probe_bytes);
    println!(
        "  Aligned ptr: {:p}, mod 8 = {}",
        aligned.as_ptr(),
        aligned.as_ptr() as usize % 8
    );
    match object::File::parse(&aligned[..]) {
        Ok(file) => {
            use object::Object;
            println!(
                "  object::File::parse: OK! Architecture: {:?}",
                file.architecture()
            );
            use object::ObjectSection;
            for section in file.sections() {
                println!(
                    "    [{}] {:?} size={}",
                    section.index().0,
                    section.name().unwrap_or("?"),
                    section.size()
                );
            }
        }
        Err(e) => println!("  object::File::parse: FAILED: {}", e),
    }

    // Test aya load with aligned data
    println!("\n=== Test 3: aya::Ebpf::load with aligned data ===");
    match aya::Ebpf::load(&aligned) {
        Ok(ebpf) => {
            println!("  OK! Programs:");
            for (name, _) in ebpf.programs() {
                println!("    - {}", name);
            }
        }
        Err(e) => {
            println!("  FAILED: {}", e);
            let mut source: Option<&dyn std::error::Error> = std::error::Error::source(&e);
            let mut i = 1;
            while let Some(s) = source {
                println!("  Cause {}: {}", i, s);
                source = std::error::Error::source(s);
                i += 1;
            }
        }
    }
}

#[cfg(not(feature = "ebpf"))]
fn main() {
    eprintln!("Requires --features ebpf");
    std::process::exit(1);
}
