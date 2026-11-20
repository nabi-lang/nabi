//! `min` / `max` of `f32` and `f64`, the same on every target: IEEE 754
//! minimumNumber / maximumNumber. A NaN operand gives the other one (NaN
//! only when both are), and `-0.0` orders below `+0.0`.

macro_rules! min_max {
    ($t:ty, $min:ident, $max:ident) => {
        #[must_use]
        pub const fn $min(x: $t, y: $t) -> $t {
            // fminnm is this already (-0.0 below +0.0, a NaN loses).
            #[cfg(target_arch = "aarch64")]
            return x.min(y);
            #[cfg(not(target_arch = "aarch64"))]
            if x.is_nan() {
                y
            } else if y.is_nan() || x < y {
                x
            } else if y < x {
                y
            } else {
                // Equal: of two zeros, the negative one.
                <$t>::from_bits(x.to_bits() | y.to_bits())
            }
        }

        #[must_use]
        pub const fn $max(x: $t, y: $t) -> $t {
            #[cfg(target_arch = "aarch64")]
            return x.max(y);
            #[cfg(not(target_arch = "aarch64"))]
            if x.is_nan() {
                y
            } else if y.is_nan() || x > y {
                x
            } else if y > x {
                y
            } else {
                <$t>::from_bits(x.to_bits() & y.to_bits())
            }
        }
    };
}

min_max!(f64, min_f64, max_f64);
min_max!(f32, min_f32, max_f32);

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact bits are the point
mod tests {
    use super::*;

    #[test]
    fn signed_zeros_and_nan() {
        assert_eq!(min_f64(0.0, -0.0).to_bits(), (-0.0f64).to_bits());
        assert_eq!(min_f64(-0.0, 0.0).to_bits(), (-0.0f64).to_bits());
        assert_eq!(max_f64(-0.0, 0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(max_f32(0.0, -0.0).to_bits(), 0.0f32.to_bits());
        assert_eq!(min_f64(f64::NAN, 1.0), 1.0);
        assert_eq!(max_f32(2.0, f32::NAN), 2.0);
        assert!(min_f64(f64::NAN, f64::NAN).is_nan());
        assert_eq!(min_f32(1.0, 2.0), 1.0);
        assert_eq!(max_f64(f64::NEG_INFINITY, -1.0), -1.0);
    }
}
