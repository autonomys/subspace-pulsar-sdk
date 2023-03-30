pub fn setup() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            "debug,parity-db=info,cranelift_codegen=info,wasmtime_cranelift=info,\
             subspace_sdk=trace,subspace_farmer=trace,subspace_service=trace,\
             subspace_farmer::utils::parity_db_store=debug,trie-cache=info,wasm_overrides=info,\
             libp2p_gossipsub::behaviour=info,wasmtime_jit=info,wasm-runtime=info"
                .parse::<tracing_subscriber::EnvFilter>()
                .expect("Env filter directives are correct"),
        )
        .with_test_writer()
        .try_init();
}
