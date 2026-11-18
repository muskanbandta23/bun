use bun_engine_number::*;
use bun_engine_standalone as _;

#[test]
fn int32() {
    for (value, expected) in [
        (0.0, 0),
        (-0.9, 0),
        (f64::NAN, 0),
        (f64::INFINITY, 0),
        (f64::MIN, 0),
        (-1.5, -1),
        (2147483648.0, i32::MIN),
        (-2147483649.0, i32::MAX),
        (4294967295.9, -1),
        (2251799813685249.5, 1),
        (-9007199254740991.0, 1),
        (1e20, 1661992960),
        (-1e20, -1661992960),
        (2f64.powi(84) - 2f64.powi(31), i32::MIN),
    ] {
        assert_eq!(to_int32(value), expected, "{value}");
        assert_eq!(to_uint32(value), expected as u32, "{value}");
    }
}

#[test]
fn exact_conversions() {
    assert_eq!(exact_int32(0.0), Some(0));
    assert_eq!(exact_int32(-2147483648.0), Some(i32::MIN));
    assert_eq!(exact_int32(2147483647.0), Some(i32::MAX));
    for value in [-0.0, 0.5, 2147483648.0, 4294967296.0, 1e300, f64::NAN] {
        assert_eq!(exact_int32(value), None, "{value}");
    }
}
