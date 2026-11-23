"""Deterministic stand-in for the TrueType rasteriser (see MockRaster in src/emu/parity_tests.rs)."""
import functools
import numpy as np
from retroimpact import glyphs as _g
import functools

# Deterministic stand-in for the TrueType rasteriser, mirrored by MockRaster in src/emu/parity_tests.rs,
# so that parity covers all emulation logic independent of font rendering.
_FACE_IDX = {"roman": 0, "sans": 1, "courier": 2, "symbols": 3}
def _eff(ch, face):
    return "symbols" if ord(ch) > 0x2FF else face
@functools.lru_cache(maxsize=None)
def _mock_bits(ch, face, spec, cols, threshold=0.5, gx=None):
    if spec is _g.DRAFT:  # draft fallbacks are baked into tables.rs from the real font
        return _real_bits(ch, face, spec, cols, threshold, gx)
    fi = _FACE_IDX[_eff(ch, face)]
    a = np.zeros((spec.rows, cols), dtype=np.uint8)
    for r in range(spec.rows):
        for c in range(cols):
            a[r, c] = 1 if ((ord(ch) * 31 + r * 7 + c * 13 + fi * 3) % 5) < 2 else 0
    return a
@functools.lru_cache(maxsize=None)
def _mock_adv(ch, face, cap_px):
    return 4.0 + (ord(ch) % 7) + cap_px * 0.5
_real_bits = _g.ttf_bits
_g.ttf_bits = _mock_bits
_g.ttf_advance_px = _mock_adv


# The original caches brushes without `extra_space_u` in the key, so ESC SP only affects glyphs that were not
# drawn before it (a latent bug). The Rust port keys the cache on it; mirror that here so parity still holds.
from retroimpact import engine as _e


def _get_brush(self, ch):
    st = self.style
    spec = self._mode_spec()
    wm = self._wm()
    key = (
        ch, spec.name, st.face, round(st.eff_cpi(), 3), st.prop, wm, st.hmul, st.vscale,
        st.emph, st.dstrike, st.italic, st.script, st.underline, st.overscore, st.reverse,
        self.family, self.extra_space_u,
    )
    hit = self.brush_ids.get(key)
    if hit is not None:
        return hit, self._adv_cache[hit]
    bid, adv = self._build_brush(ch, spec, wm)
    self.brush_ids[key] = bid
    self._adv_cache[bid] = adv
    return bid, adv


_e.Engine.get_brush = _get_brush
