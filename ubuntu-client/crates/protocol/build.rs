fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::Path::new("../../../proto");
    prost_build::compile_protos(
        &[
            proto_dir.join("control.proto"),
            proto_dir.join("cursor.proto"),
            proto_dir.join("input.proto"),
        ],
        &[proto_dir],
    )?;
    Ok(())
}
