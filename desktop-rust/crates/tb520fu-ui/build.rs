// SPDX-License-Identifier: MIT
use std::env;
use std::fs;
use std::path::PathBuf;

use naga::back::spv;
use naga::front::wgsl;
use naga::valid::{Capabilities, ValidationFlags, Validator};

fn compile_wgsl_entry(
    wgsl_path: &str,
    stage: naga::ShaderStage,
    entry: &str,
    out_file: &str,
) {
    println!("cargo:rerun-if-changed={wgsl_path}");
    let src = fs::read_to_string(wgsl_path).expect("read wgsl");
    let module = wgsl::parse_str(&src)
        .unwrap_or_else(|e| panic!("WGSL parse error in {wgsl_path}:\n{}", e.emit_to_string(&src)));
    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::empty());
    let info = validator
        .validate(&module)
        .unwrap_or_else(|e| panic!("WGSL validation error in {wgsl_path}: {e:?}"));

    let mut opts = spv::Options::default();
    opts.lang_version = (1, 0);
    opts.flags.remove(spv::WriterFlags::ADJUST_COORDINATE_SPACE);

    let pipe_opts = spv::PipelineOptions {
        shader_stage: stage,
        entry_point: entry.to_string(),
    };
    let words = spv::write_vec(&module, &info, &opts, Some(&pipe_opts))
        .unwrap_or_else(|e| panic!("SPIR-V codegen error in {wgsl_path}::{entry}: {e:?}"));

    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join(out_file), bytes).expect("write spv");
}

fn main() {
    compile_wgsl_entry(
        "shaders/uber_sdf.wgsl",
        naga::ShaderStage::Vertex,
        "vs_main",
        "uber_sdf_vs.spv",
    );
    compile_wgsl_entry(
        "shaders/uber_sdf.wgsl",
        naga::ShaderStage::Fragment,
        "fs_main",
        "uber_sdf_fs.spv",
    );
    compile_wgsl_entry(
        "shaders/kawase_blur.wgsl",
        naga::ShaderStage::Vertex,
        "vs_fullscreen",
        "kawase_vs.spv",
    );
    compile_wgsl_entry(
        "shaders/kawase_blur.wgsl",
        naga::ShaderStage::Fragment,
        "fs_down",
        "kawase_down_fs.spv",
    );
    compile_wgsl_entry(
        "shaders/kawase_blur.wgsl",
        naga::ShaderStage::Fragment,
        "fs_up",
        "kawase_up_fs.spv",
    );
}
