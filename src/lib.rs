//! RetroImpact: raw 1980s dot-matrix printer data -> pages that look printed on one.
//!
//! * [`emu`]: printer emulation (Epson/IBM/Star, ImageWriter, Commodore MPS, Okimate) -> dots, no GUI and no GPU
//! * [`render`]: the wgpu renderer (dots -> ribbon, paper, impact shading)
//! * [`export`]: PNG / PDF / TIFF writers fed band by band from the renderer
//! * [`fontraster`]: TrueType rasteriser used for the NLQ / letter-quality glyphs
pub mod emu;
pub mod export;
pub mod fontraster;
pub mod render;
