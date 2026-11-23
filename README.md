# RustyDotMatrix

Proof of concept for a retro dot matrix printer viewer/converter.
Rust + [egui](https://github.com/emilk/egui) (eframe) with the **wgpu** renderer.

What it does today:

* Opens a raw printer data file (button, or drag & drop onto the window)
* Interprets a subset of **Epson ESC/P (9-pin)** into individual dots
  * text (10 / 12 / 15 cpi, condensed, double width, emphasized, double strike, underline)
  * line spacing (`ESC 0 1 2 3 A J`), form feed, auto page breaks
  * bit images: `ESC K/L/Y/Z`, `ESC *` (8-pin and 24-pin modes)
* Draws every dot on virtual paper with zoom and pan

Not done yet: PDF/TIFF export, Star / Commodore / ImageWriter dialects, code pages
(bytes >= 0x80 print as blank cells), NLQ, margins, tab stops, the "crappy old printer" effects.

## Controls

| Action | How |
| --- | --- |
| Pan | drag with the mouse |
| Scroll | mouse wheel |
| Zoom | Ctrl + wheel, pinch, or the − / + / 100% / Fit width buttons |
| Open | Open… button, or drop a file on the window |

`samples/demo.prn` is a small ESC/P test file; the **Demo** button loads it.

## Build

```
cargo run --release
```

On Debian/Ubuntu you may need:
`sudo apt-get install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev`

GitHub Actions builds Windows, Linux and macOS (Apple Silicon) binaries on every push;
download them from the workflow run's **Artifacts**.

## Credits

The text glyphs come from the public-domain X11 "Misc Fixed" 6x9 bitmap font.
