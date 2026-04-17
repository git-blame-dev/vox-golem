#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputSample {
    I16(i16),
    U16(u16),
    F32(f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SampleConversionError {
    F32OutOfRange { value: f32 },
}

pub fn normalize_sample(sample: InputSample) -> Result<f32, SampleConversionError> {
    match sample {
        InputSample::I16(value) => Ok(normalize_i16_sample(value)),
        InputSample::U16(value) => Ok(normalize_u16_sample(value)),
        InputSample::F32(value) => normalize_f32_sample(value),
    }
}

pub fn normalize_i16_sample(value: i16) -> f32 {
    // i16 uses an asymmetric two's-complement range (-32768..=32767).
    // We map both endpoints explicitly to avoid values below -1.0.
    if value == i16::MIN {
        -1.0
    } else {
        value as f32 / i16::MAX as f32
    }
}

pub fn normalize_u16_sample(value: u16) -> f32 {
    let normalized_zero_to_one = value as f32 / u16::MAX as f32;
    (normalized_zero_to_one * 2.0) - 1.0
}

pub fn normalize_f32_sample(value: f32) -> Result<f32, SampleConversionError> {
    if (-1.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(SampleConversionError::F32OutOfRange { value })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_f32_sample, normalize_i16_sample, normalize_sample, normalize_u16_sample,
        InputSample, SampleConversionError,
    };

    fn approx_equal(left: f32, right: f32) {
        assert!((left - right).abs() <= 0.000_1);
    }

    #[test]
    fn normalizes_i16_endpoints_and_zero() {
        approx_equal(normalize_i16_sample(i16::MIN), -1.0);
        approx_equal(normalize_i16_sample(0), 0.0);
        approx_equal(normalize_i16_sample(i16::MAX), 1.0);
    }

    #[test]
    fn normalizes_u16_endpoints_and_center() {
        approx_equal(normalize_u16_sample(0), -1.0);
        approx_equal(normalize_u16_sample(u16::MAX), 1.0);
        approx_equal(normalize_u16_sample(32_768), 0.000_015_26);
    }

    #[test]
    fn rejects_out_of_range_f32_samples() {
        assert_eq!(
            normalize_f32_sample(1.25),
            Err(SampleConversionError::F32OutOfRange { value: 1.25 })
        );
    }

    #[test]
    fn normalizes_mixed_input_samples() {
        let i16_result = normalize_sample(InputSample::I16(16_384));
        let u16_result = normalize_sample(InputSample::U16(16_384));
        let f32_result = normalize_sample(InputSample::F32(0.5));

        assert!(matches!(i16_result, Ok(value) if value > 0.49 && value < 0.51));
        assert!(matches!(u16_result, Ok(value) if value < -0.49 && value > -0.51));
        assert_eq!(f32_result, Ok(0.5));
    }
}
