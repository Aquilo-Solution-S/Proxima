fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../proto";
    tonic_prost_build::configure()
        .build_client(false)
        .build_server(true)
        .compile_protos(
            &[
                "../../proto/proxima/v1/messages.proto",
                "../../proto/proxima/v1/engine.proto",
            ],
            &[proto_root],
        )?;
    println!("cargo:rerun-if-changed={proto_root}");
    Ok(())
}
