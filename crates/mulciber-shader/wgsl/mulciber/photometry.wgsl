#define_import_path mulciber::photometry

// Photometric light units, punctual-light falloff and camera exposure, following Filament
// ("Physically Based Rendering in Filament", lighting and exposure chapters).
//
// Units are SI and photometric throughout; distances are in metres:
// - luminous flux Φ in lumens (lm): how lamps are authored. A 60 W incandescent bulb gives about
//   800 lm.
// - luminous intensity I in candelas (cd = lm/sr): what point and spot lights shade with.
// - illuminance E in lux (lx = lm/m²): what reaches a surface. Directional lights such as the sun
//   are authored in lux (around 100 000 lx at noon, a few hundred at sunset).
// - luminance L in nits (cd/m²): skies, emissive surfaces, calibrated HDRIs, and every shaded
//   value. A BRDF in 1/sr times illuminance in lux gives luminance in nits.
// Colours are linear and dimensionless; they scale these quantities per channel.
//
// Exposure maps scene luminance to the display range. Multiply by it as early as possible
// ("pre-exposure", as Filament does for light intensities) so values stay small in 16-bit
// targets, then tone map.

const PI: f32 = 3.14159265358979;

// Punctual lights are treated as 1 cm spheres: the squared distance never drops below 1 cm².
const MIN_DISTANCE_SQUARED: f32 = 0.0001;

// Luminous intensity in candelas of a point light emitting `lumens` evenly over the sphere: Φ/4π.
fn point_light_intensity(lumens: f32) -> f32 {
    return lumens / (4.0 * PI);
}

// Luminous intensity in candelas of a spot light emitting `lumens`, as Filament's default
// (unfocused) spot: Φ/π, whatever its cone. Narrowing the cone then darkens the lit area
// instead of concentrating the same flux into it, so an artist can change the angle without the
// light getting brighter. The flux is not conserved; a focused spot is.
fn spot_light_intensity(lumens: f32) -> f32 {
    return lumens / PI;
}

// Luminous intensity in candelas of a focused spot: `lumens` spread over the cone of half-angle
// θ_outer, Φ / (2π (1 − cos θ_outer)). Narrowing the cone concentrates the light, like a real
// reflector lamp.
fn focused_spot_light_intensity(lumens: f32, cos_outer: f32) -> f32 {
    return lumens / (2.0 * PI * (1.0 - cos_outer));
}

// Filament's smooth window to the light's range r > 0: saturate(1 − (d/r)⁴)². It is 1 well
// inside the range and reaches 0 at d = r with zero slope, so the light ends without a seam.
fn range_window(distance_squared: f32, range: f32) -> f32 {
    let ratio = distance_squared / (range * range);
    let window = saturate(1.0 - ratio * ratio);
    return window * window;
}

// Inverse-square falloff with the range window: saturate(1 − (d/r)⁴)² / max(d², 0.01²), in
// 1/m². Multiply by an intensity in candelas for illuminance in lux.
fn distance_attenuation(distance_squared: f32, range: f32) -> f32 {
    return range_window(distance_squared, range) / max(distance_squared, MIN_DISTANCE_SQUARED);
}

// Illuminance in lux at squared distance d² from a punctual light of `intensity` candelas and
// range r: E = I / max(d², 0.01²) · saturate(1 − (d/r)⁴)². This is perpendicular illuminance;
// the BRDF's caller multiplies by n·l.
fn punctual_illuminance(intensity: f32, distance_squared: f32, range: f32) -> f32 {
    return intensity * distance_attenuation(distance_squared, range);
}

// Filament's spot cone falloff, squared for a smoother edge: 1 inside the inner cone, 0 outside
// the outer one. `cos_angle` is the cosine between the spot axis and the direction from the light
// to the surface.
fn spot_angle_attenuation(cos_angle: f32, cos_inner: f32, cos_outer: f32) -> f32 {
    let scale = 1.0 / max(cos_inner - cos_outer, 1.0 / 1024.0);
    let attenuation = saturate((cos_angle - cos_outer) * scale);
    return attenuation * attenuation;
}

// EV100 of camera settings: aperture N (f-number), shutter time t in seconds and ISO S,
// log2(N² / t · 100 / S).
fn ev100_from_camera(aperture: f32, shutter_seconds: f32, iso: f32) -> f32 {
    return log2(aperture * aperture / shutter_seconds * 100.0 / iso);
}

// EV100 that meters an average scene luminance in nits, with the reflected-light meter constant
// K = 12.5: log2(L · 100 / 12.5). For later auto-exposure.
fn ev100_from_luminance(average_luminance: f32) -> f32 {
    return log2(average_luminance * 100.0 / 12.5);
}

// Exposure for EV100: 1 / (1.2 · 2^EV100), the saturation-based sensitivity of a camera at
// ISO 100 with the 1.2 lens and vignetting factor (q = 0.65). Luminance times exposure is the
// value handed to the tone mapper, where 1 is the brightest a pixel can be without clipping.
fn exposure_from_ev100(ev100: f32) -> f32 {
    return 1.0 / (1.2 * exp2(ev100));
}

// Pre-exposes a luminance or a light's colour-times-intensity.
fn pre_expose(value: vec3<f32>, exposure: f32) -> vec3<f32> {
    return value * exposure;
}

// Pre-exposes a scalar intensity (candelas) or illuminance (lux).
fn pre_expose_intensity(intensity: f32, exposure: f32) -> f32 {
    return intensity * exposure;
}
