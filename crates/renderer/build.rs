use wesl::Wesl;

fn main() {
    let mut wesl = Wesl::new("shaders");
    wesl.use_sourcemap(true);
    wesl.set_options(wesl::CompileOptions {
        imports: true,
        condcomp: false,
        generics: false,
        strip: true,
        lower: true,
        validate: true,
        ..Default::default()
    });

    wesl.build_artifact(&"package::clear".parse().unwrap(), "clear");
    wesl.build_artifact(&"package::xfb_blit".parse().unwrap(), "xfb_blit");
    wesl.build_artifact(&"package::color_blit".parse().unwrap(), "color_blit");
    wesl.build_artifact(&"package::depth_blit".parse().unwrap(), "depth_blit");
    wesl.build_artifact(&"package::color_convert".parse().unwrap(), "color_convert");
    wesl.build_artifact(&"package::depth_convert".parse().unwrap(), "depth_convert");
    wesl.build_artifact(&"package::depth_resolve".parse().unwrap(), "depth_resolve");

    // When targeting WebGPU, generate push-constant-free shader variants.
    // `var<push_constant>` is a Vulkan extension absent from browser WebGPU;
    // we replace it with a `@group(N) @binding(0) var<uniform>` binding.
    if std::env::var("CARGO_FEATURE_WEBGPU").is_ok() {
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let out_dir = std::path::Path::new(&out_dir);

        // (shader_artifact_name, uniform_group_index)
        // The group index is chosen to be the first unused bind-group for
        // that particular pipeline so it doesn't collide with existing groups.
        let shaders: &[(&str, u32)] = &[
            // clear: no existing groups → group 0
            ("clear", 0),
            // blit shaders: group 0 = texture + sampler → group 1
            ("xfb_blit", 1),
            ("color_blit", 1),
            ("depth_blit", 1),
            ("depth_resolve", 1),
            // convert (compute): group 0 = input + output → group 1
            ("color_convert", 1),
            ("depth_convert", 1),
        ];

        for (name, group) in shaders {
            let src_path = out_dir.join(format!("{name}.wgsl"));
            let wgsl = std::fs::read_to_string(&src_path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", src_path.display()));

            // Replace `var<push_constant> <name>: <type>;` with the uniform
            // binding. The compiled WGSL uses either `var<push_constant>` (for
            // variables bound in module scope) which we replace uniformly.
            let patched = wgsl.replace(
                "var<push_constant>",
                &format!("@group({group}) @binding(0) var<uniform>"),
            );

            let dst_path = out_dir.join(format!("{name}_webgpu.wgsl"));
            std::fs::write(&dst_path, patched)
                .unwrap_or_else(|e| panic!("failed to write {}: {e}", dst_path.display()));
        }
    }
}

