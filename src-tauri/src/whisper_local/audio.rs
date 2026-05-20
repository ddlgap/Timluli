use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// Converts i16 PCM samples to f32 in [-1.0, 1.0].
#[allow(dead_code)]
pub fn int16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| s as f32 / 32768.0).collect()
}

/// Mixes stereo (interleaved L,R) to mono by averaging channels.
#[allow(dead_code)]
pub fn stereo_to_mono(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(2)
        .map(|c| if c.len() == 2 { (c[0] + c[1]) * 0.5 } else { c[0] })
        .collect()
}

/// Resamples mono f32 audio from `from_rate` Hz to 16 000 Hz.
/// Returns the input unchanged if already 16 kHz.
#[allow(dead_code)]
pub fn resample_to_16k(samples: &[f32], from_rate: u32) -> Result<Vec<f32>, String> {
    if from_rate == 16_000 {
        return Ok(samples.to_vec());
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let ratio = 16_000.0 / from_rate as f64;
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut resampler =
        SincFixedIn::<f32>::new(ratio, 2.0, params, samples.len(), 1)
            .map_err(|e| format!("אתחול resampler: {e}"))?;

    let output = resampler
        .process(&[samples.to_vec()], None)
        .map_err(|e| format!("resampling: {e}"))?;

    Ok(output.into_iter().next().unwrap_or_default())
}

/// Validates that audio is in the expected format for whisper.cpp.
/// whisper.cpp requires 16 kHz mono f32 PCM in [-1.0, 1.0].
pub fn validate(samples: &[f32]) -> Result<(), String> {
    if samples.is_empty() {
        return Err("מאגר שמע ריק".into());
    }
    const MAX_SAMPLES: usize = 16_000 * 30; // 30 seconds
    if samples.len() > MAX_SAMPLES {
        return Err(format!(
            "השמע ארוך מדי: {} שניות (מקסימום 30)",
            samples.len() / 16_000
        ));
    }
    Ok(())
}
