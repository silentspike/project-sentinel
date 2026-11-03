#![cfg(feature = "wasm")]

use wasmtime::{Config, Engine, Finality, Instance, Module, NoneRef, Store, StructType, I31};

#[test]
fn same_engine_struct_supertype_remains_supported() {
    let engine = Engine::default();
    let parent =
        StructType::with_finality_and_supertype(&engine, Finality::NonFinal, None, []).unwrap();
    assert!(
        StructType::with_finality_and_supertype(&engine, Finality::Final, Some(&parent), [])
            .is_ok()
    );
}

#[test]
#[should_panic(expected = "supertype.comes_from_same_engine(engine)")]
fn foreign_engine_struct_supertype_is_rejected_at_the_boundary() {
    let local = Engine::default();
    let foreign = Engine::default();
    let parent =
        StructType::with_finality_and_supertype(&foreign, Finality::NonFinal, None, []).unwrap();
    let _ = StructType::with_finality_and_supertype(&local, Finality::Final, Some(&parent), []);
}

#[test]
fn typed_gc_arguments_fail_with_errors_without_panicking() -> wasmtime::Result<()> {
    let mut config = Config::new();
    config.wasm_gc(true);
    let engine = Engine::new(&config)?;
    let module = Module::new(
        &engine,
        r#"(module
            (type $record (struct))
            (func (export "optional") (param (ref null $record)))
            (func (export "required") (param (ref $record))))"#,
    )?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[])?;
    let optional = instance.get_typed_func::<Option<I31>, ()>(&mut store, "optional")?;
    optional.call(&mut store, None)?;
    assert!(optional
        .call(&mut store, Some(I31::new_i32(5).unwrap()))
        .is_err());
    let required = instance.get_typed_func::<Option<NoneRef>, ()>(&mut store, "required")?;
    assert!(required.call(&mut store, None).is_err());
    Ok(())
}
