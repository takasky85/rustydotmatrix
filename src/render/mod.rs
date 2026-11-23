//! GPU renderer: see `render.wgsl` for the passes and `gpu.rs` for the plumbing.
pub mod dots;
pub mod gpu;

pub use dots::{build_sheet, dots_key, GpuDot, SheetDots};
pub use gpu::{displayed_size, margin_px, supersample, Region, Renderer, SheetBuf, SHADER_SRC};

#[cfg(test)]
mod tests {
    use super::*;

    /// The shader must parse and validate: this catches WGSL mistakes without needing a GPU.
    #[test]
    fn shader_validates() {
        let module = naga::front::wgsl::parse_str(SHADER_SRC).unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER_SRC)));
        let mut v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
        if let Err(e) = v.validate(&module) {
            panic!("{}", e.emit_to_string(SHADER_SRC));
        }
        let names: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
        for n in ["vs_cov", "fs_cov", "vs_full", "fs_ink", "fs_blur_h", "fs_blur_v", "fs_final"] {
            assert!(names.contains(&n), "missing entry point {n}");
        }
    }

    #[test]
    fn params_layout_matches_wgsl() {
        assert_eq!(std::mem::size_of::<gpu::Params>(), 14 * 16);
    }
}
