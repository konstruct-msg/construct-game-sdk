// The .proto is the one authority for the ABI; the Rust types are generated from it on
// every build. protox compiles it in Rust, so no `protoc` binary is needed — not here,
// and not in construct-core, which builds this crate through construct-games-host.

fn main() {
    let proto = "proto/construct_game_abi.proto";
    println!("cargo:rerun-if-changed={proto}");
    let descriptors = protox::compile([proto], ["proto"]).expect("compile ABI proto");
    prost_build::Config::new()
        .compile_fds(descriptors)
        .expect("generate ABI types");
}
