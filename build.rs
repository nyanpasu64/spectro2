use anyhow::*;
use glob::glob;
use std::path::PathBuf;
use std::{
    ffi::OsStr,
    fs::{create_dir_all, read_to_string, write},
};

struct ShaderData {
    src: String,
    src_path: PathBuf,
    spv_path: PathBuf,
    kind: shaderc::ShaderKind,
}

// this should be a &Path but it's impossible.
fn out_dir() -> PathBuf {
    "generated".to_owned().into()
}

impl ShaderData {
    pub fn load(src_path: PathBuf) -> Result<Self> {
        let extension = src_path
            .extension()
            .context("File has no extension")?
            .to_str();
        let kind = match extension {
            Some("vert") => shaderc::ShaderKind::Vertex,
            Some("frag") => shaderc::ShaderKind::Fragment,
            Some("comp") => shaderc::ShaderKind::Compute,
            _ => bail!("Unsupported shader: {}", src_path.display()),
        };
        let extension = extension.unwrap();

        let src = read_to_string(src_path.clone())?;

        let mut spv_path = PathBuf::new();
        spv_path.push(out_dir());

        let name: &OsStr = src_path
            .file_name()
            .with_context(|| format!("Path {} has no name", src_path.display()))?;
        let mut name = PathBuf::from(name);
        name.set_extension(extension.to_owned() + ".spv");
        spv_path.push(name);

        Ok(Self {
            src,
            src_path,
            spv_path,
            kind,
        })
    }
}

fn main() -> Result<()> {
    create_dir_all(out_dir()).context("Failed to create output dir")?;

    let paths = ["./src/**/*.vert", "./src/**/*.frag", "./src/**/*.comp"];

    // Tell Cargo when to rebuild
    for path in &paths {
        println!("cargo:rerun-if-changed={}", path);
    }

    let mut shader_paths = vec![];

    // Collect all shaders recursively within /src/
    for path in &paths {
        shader_paths.push(glob(path)?);
    }

    // This could be parallelized
    let mut shaders = vec![];
    for glob_results in shader_paths {
        for glob_result in glob_results {
            shaders.push(ShaderData::load(glob_result?)?);
        }
    }
    let shaders = shaders;

    let mut compiler = shaderc::Compiler::new().context("Unable to create shader compiler")?;

    // This can't be parallelized. The [shaderc::Compiler] is not
    // thread safe. Also, it creates a lot of resources. You could
    // spawn multiple processes to handle this, but it would probably
    // be better just to only compile shaders that have been changed
    // recently.
    for shader in shaders {
        let compiled = compiler.compile_into_spirv(
            &shader.src,
            shader.kind,
            &shader.src_path.to_str().unwrap(),
            "main",
            None,
        )?;
        write(shader.spv_path, compiled.as_binary_u8())?;
    }

    Ok(())
}
