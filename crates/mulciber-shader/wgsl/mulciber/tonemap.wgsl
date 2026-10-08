#define_import_path mulciber::tonemap

// Tone mapping from exposed scene luminance (see mulciber::photometry::exposure_from_ev100) to
// display-linear values in [0, 1), before the sRGB encode.

// Values whose brightest channel is at or below this pass through unchanged.
const SHOULDER_START: f32 = 0.6;

// Isle of Rán's hue-preserving shoulder. Negative input is clamped to zero. Below the shoulder the
// colour is unchanged, so mid-tones keep their exposure. Above it the brightest channel p maps to
// 0.6 + 0.4 (p − 0.6) / (p − 0.2), and the whole colour scales by the same factor, so channel
// ratios (hue and saturation) are kept instead of highlights drifting to white or clipping per
// channel. The curve meets the identity at 0.6 with slope 1, rises monotonically, and approaches
// 1 without reaching it: p = 1 gives 0.8, 2 gives 0.911, 10 gives 0.98.
fn hue_preserving_shoulder(radiance: vec3<f32>) -> vec3<f32> {
    let positive = max(radiance, vec3<f32>(0.0));
    let peak = max(positive.x, max(positive.y, positive.z));
    if peak <= SHOULDER_START {
        return positive;
    }
    let headroom = 1.0 - SHOULDER_START;
    let shoulder = SHOULDER_START + headroom * (peak - SHOULDER_START) / (peak - SHOULDER_START + headroom);
    return positive * (shoulder / peak);
}
