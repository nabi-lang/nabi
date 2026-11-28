# raytracer

A small path tracer: spheres on a checkered ground plane, diffuse, metal
and glass materials, 100 jittered samples per pixel (anti-aliasing) and
gamma correction. Rows are spread over one OS thread per CPU
(`std::thread`), and the image is written as a binary PPM.

It shows:

- `Vec3` arithmetic through the `core::ops` traits: `+ - *`, unary `-`,
  and `.*` (`DotMul`) for the component-wise product of colors.
- Unicode operators from core: `a ⋅ b` (`ops::Dot`), `a × b`
  (`ops::Cross`) and prefix `√x` (`ops::Sqrt`).
- A user-defined postfix operator, registered on a trait method:
  `#[postfix("°", like = "?")]`, so the field of view reads `30.0°`.
- Materials and shapes as enums with exhaustive `match`.
- `T | none` results with `let ... else` for misses and absorbed rays.
- A frozen scene list shared read-only by every worker thread.

## Run

    nabi run --release -- 400 out.ppm

Arguments: image width (default 400; height is 9/16 of it) and output
path (default `out.ppm`). Most image viewers open PPM; to convert, use
e.g. `magick out.ppm out.png` or, on macOS,
`sips -s format png out.ppm --out out.png`.
