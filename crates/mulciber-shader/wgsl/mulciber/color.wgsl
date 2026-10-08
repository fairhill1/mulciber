#define_import_path mulciber::color

// Colour-space helpers shipped with mulciber-shader. Import with `#import mulciber::color` and
// call `mulciber::color::luminance(...)`, or `#import mulciber::color::{luminance}`.
//
// The sRGB transfer function is the piecewise IEC 61966-2-1 curve, not a 2.2 gamma. Values at or
// below the linear segment's threshold, including negative values, stay on the linear segment, so
// no input produces NaN. Linear values above 1.0 (HDR) encode above 1.0.

// One sRGB-encoded channel to linear light.
fn srgb_to_linear_channel(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

// One linear-light channel to its sRGB encoding.
fn linear_to_srgb_channel(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        return linear * 12.92;
    }
    return 1.055 * pow(linear, 1.0 / 2.4) - 0.055;
}

// An sRGB-encoded colour to linear light, per channel.
fn srgb_to_linear(encoded: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        srgb_to_linear_channel(encoded.x),
        srgb_to_linear_channel(encoded.y),
        srgb_to_linear_channel(encoded.z),
    );
}

// A linear-light colour to its sRGB encoding, per channel.
fn linear_to_srgb(linear: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        linear_to_srgb_channel(linear.x),
        linear_to_srgb_channel(linear.y),
        linear_to_srgb_channel(linear.z),
    );
}

// Relative luminance of a linear-light colour with Rec. 709 / sRGB primaries (BT.709 weights).
fn luminance(linear: vec3<f32>) -> f32 {
    return dot(linear, vec3<f32>(0.2126, 0.7152, 0.0722));
}
