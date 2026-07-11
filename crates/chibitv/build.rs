fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc is available");

    // SAFETY: Cargo runs this build script in its own process, before any
    // threads are created. The variable only configures connectrpc-build.
    unsafe { std::env::set_var("PROTOC", protoc) };

    connectrpc_build::Config::new()
        .files(&["../../proto/chibitv/v1/chibitv.proto"])
        .includes(&["../../proto"])
        .include_file("_connectrpc.rs")
        .compile()
        .expect("Connect RPC code generation succeeds");
}
