# orbit

The Moon about the Earth for one year, as a scaled two-body problem
integrated by `ode`'s adaptive DOP853, with `units` quantities at the
edges and the result drawn by `plot` to `orbit.svg`.

- Inputs are quantities: `5.9722e24kg`, `363300km`, `1.082km / 1s`,
  `365.25d`. The integrator works in perigee distances and the natural
  time unit `√(rp³ / μ)`, which only compiles because the dimension
  under the root is s².
- Three events on the dense output: upward crossings of the x axis give
  the period, the zeros of the radial velocity give perigee and apogee.
- The period is checked against Kepler's third law (vis-viva for the
  semi-major axis), and the integrator by the drift of the orbital
  energy over all 502 steps.
- Outputs convert with `->`: `period -> d`, `a -> km`.
- Arrays use the dot operators (`.*`, `./`, `.+`, `.**`), which fuse
  into one loop.

## Run

```sh
nabi run --release
```

Output:

```
Moon about Earth, 365.25 d, DOP853 at rtol 1e-11
  steps               502 (7592 evaluations)
  orbits              13.40
  period, measured    27.2495 d
  period, Kepler III  27.2495 d
  semi-major axis     384070 km
  perigee             363300 km
  apogee              404841 km
  eccentricity        0.05408
  orbital energy      -3.8099e28 J
  max energy drift    1.30e-11
wrote orbit.svg
```

`orbit.svg` has two panels: the orbit with the Earth at a focus, and
the relative energy error, which oscillates once per orbit and drifts
by about 1e-11 a year (DOP853 is not symplectic; `ode::symplectic`
would keep it bounded). The last digits of the drift differ between
aarch64 and x86_64.
