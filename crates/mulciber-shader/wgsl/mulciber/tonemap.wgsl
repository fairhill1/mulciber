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

// Khronos PBR Neutral (Emmett Lalish for the Khronos 3D Commerce group, 2024; the specification
// and reference are at https://github.com/KhronosGroup/ToneMapping/tree/main/PBR_Neutral, under
// Apache-2.0), as Bevy 0.20 ships it. A toe takes up to 0.04 off every channel by the darkest
// one, quadratically near black, which deepens the shadows and the colour in them. Past the toe
// a colour whose brightest channel is under 0.76 passes through. Above it the brightest channel
// is compressed toward 1, 1 − d² / (p + d − 0.76) with d = 0.24, the colour scaled with it as
// `hue_preserving_shoulder` does; and the colour is then mixed toward white by how much it was
// compressed, so very bright light whitens as it does on film and to the eye instead of staying
// saturated all the way up, where its gradients would flatten into one band.
const PBR_NEUTRAL_START: f32 = 0.76;
const PBR_NEUTRAL_DESATURATION: f32 = 0.15;
// Where the toe's quadratic meets its constant offset: x − 6.25x² is 0.04 at 0.08, slope 0.
const PBR_NEUTRAL_TOE_END: f32 = 0.08;
const PBR_NEUTRAL_OFFSET: f32 = 0.04;

// How much the toe takes off every channel of a colour whose darkest channel is `darkest`.
fn pbr_neutral_toe(darkest: f32) -> f32 {
    if darkest < PBR_NEUTRAL_TOE_END {
        return darkest - 6.25 * darkest * darkest;
    }
    return PBR_NEUTRAL_OFFSET;
}

// How far the compression mixes a colour toward white: none until `peak` (past the toe) is
// compressed to `compressed`, approaching all of it.
fn pbr_neutral_whitening(peak: f32, compressed: f32) -> f32 {
    return 1.0 - 1.0 / (PBR_NEUTRAL_DESATURATION * (peak - compressed) + 1.0);
}

// Exposed scene luminance to display-linear [0, 1), as above. Negative input is clamped to zero.
fn pbr_neutral(radiance: vec3<f32>) -> vec3<f32> {
    let positive = max(radiance, vec3<f32>(0.0));
    let darkest = min(positive.x, min(positive.y, positive.z));
    let toed = positive - vec3<f32>(pbr_neutral_toe(darkest));
    let peak = max(toed.x, max(toed.y, toed.z));
    if peak < PBR_NEUTRAL_START {
        return toed;
    }
    let headroom = 1.0 - PBR_NEUTRAL_START;
    let compressed = 1.0 - headroom * headroom / (peak + headroom - PBR_NEUTRAL_START);
    let whitening = pbr_neutral_whitening(peak, compressed);
    return mix(toed * (compressed / peak), vec3<f32>(compressed), whitening);
}

// The exposed colour `pbr_neutral` shows as display-linear `shown`, exactly: the shown colour's
// brightest channel is the compressed peak, which gives back the peak and how far it was
// whitened; undoing the whitening and the compression gives the colour past the toe, whose
// darkest channel gives back the toe. A brightest channel of 1 (which the curve never reaches)
// is taken as just under it.
fn pbr_neutral_inverse(shown: vec3<f32>) -> vec3<f32> {
    let positive = max(shown, vec3<f32>(0.0));
    let compressed = min(max(positive.x, max(positive.y, positive.z)), 0.9999);
    var toed = positive;
    if compressed >= PBR_NEUTRAL_START {
        let headroom = 1.0 - PBR_NEUTRAL_START;
        let peak = PBR_NEUTRAL_START - headroom + headroom * headroom / (1.0 - compressed);
        let whitening = pbr_neutral_whitening(peak, compressed);
        toed = (positive - vec3<f32>(whitening * compressed)) * (peak / ((1.0 - whitening) * compressed));
    }
    // The toe left the darkest channel at x − toe(x): 6.25x² below the toe's end, x − 0.04 above.
    let left = max(min(toed.x, min(toed.y, toed.z)), 0.0);
    var darkest = left + PBR_NEUTRAL_OFFSET;
    if left < PBR_NEUTRAL_OFFSET {
        darkest = sqrt(left / 6.25);
    }
    return max(toed + vec3<f32>(darkest - left), vec3<f32>(0.0));
}

// `hue_preserving_shoulder` with PBR Neutral's path to white: unchanged where the shoulder leaves a
// colour be (its brightest channel at most 0.6), and above it mixed toward white by how far the
// shoulder compressed it, by PBR Neutral's measure, `pbr_neutral_whitening`. Very bright light
// whitens as it does on film, where the shoulder alone kept it saturated up to white and its
// gradients flattened into one band; everything below keeps the shoulder's look, toe and all.
fn whitening_shoulder(radiance: vec3<f32>) -> vec3<f32> {
    let positive = max(radiance, vec3<f32>(0.0));
    let peak = max(positive.x, max(positive.y, positive.z));
    let shouldered = hue_preserving_shoulder(positive);
    let compressed = max(shouldered.x, max(shouldered.y, shouldered.z));
    return mix(shouldered, vec3<f32>(compressed), pbr_neutral_whitening(peak, compressed));
}

// The exposed colour `whitening_shoulder` shows as display-linear `shown`, exactly: the shown
// brightest channel is the shoulder's, which gives back the peak, `(0.36 − 0.2n) / (1 − n)`, and
// how far it was whitened. A brightest channel of 1 is taken as just under it.
fn whitening_shoulder_inverse(shown: vec3<f32>) -> vec3<f32> {
    let positive = max(shown, vec3<f32>(0.0));
    let compressed = min(max(positive.x, max(positive.y, positive.z)), 0.9999);
    if compressed <= SHOULDER_START {
        return positive;
    }
    let peak = (0.36 - 0.2 * compressed) / (1.0 - compressed);
    let whitening = pbr_neutral_whitening(peak, compressed);
    let shouldered = (positive - vec3<f32>(whitening * compressed)) / (1.0 - whitening);
    return max(shouldered * (peak / compressed), vec3<f32>(0.0));
}
