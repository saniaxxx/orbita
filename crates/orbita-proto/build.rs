use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest.join("protos");
    if !proto_dir.is_dir() {
        return Err(format!(
            "missing {}; the proto schemas are vendored in crates/orbita-proto/protos",
            proto_dir.display()
        )
        .into());
    }

    println!("cargo:rerun-if-changed={}", proto_dir.display());
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: build scripts are single-threaded when this runs.
    unsafe {
        env::set_var("PROTOC", protoc);
    }

    let mut files = Vec::new();
    collect(&proto_dir, &mut files)?;
    if files.is_empty() {
        return Err("no .proto files found".into());
    }

    let mut config = prost_build::Config::new();
    config.include_file("protos.rs");
    config.compile_protos(&files, &[proto_dir])?;
    Ok(())
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "proto")
        {
            // macOS keeps AppleDouble metadata next to files on some volumes;
            // protoc must not see `._name.proto`.
            let apple_double = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("._"));
            if !apple_double {
                files.push(path);
            }
        }
    }
    Ok(())
}
