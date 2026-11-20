#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Liquid Glass: a (lightly) blurred backdrop drawn as a slab of glass.
// Near the edge the slab curves away, so what is behind it refracts towards
// the middle (a convex bezel), splits slightly into colours, and catches a
// specular highlight from a light above-left.

// size of the element in physical pixels
uniform vec2 size;
// corner radius in physical pixels
uniform float radius;
// which corners to round: (top-left, top-right, bottom-right, bottom-left)
uniform vec4 corners;
// crop of the texture this element samples (normalized)
uniform vec2 src_off;
uniform vec2 src_scale;
// (unused here; same interface as rounded.frag)
uniform vec4 rim;
uniform float rim_width;
// x: bezel width (px), y: refraction depth (px), z: specular strength,
// w: transparency (0 = frosted .. 1 = clear)
uniform vec4 glass;

// Signed distance (px) to the continuous (squircle) rounded box, as in
// rounded.frag. Negative inside.
float box_sdf(vec2 p) {
    float r = min(radius, min(size.x, size.y) * 0.5);
    const float N = 2.6;
    const float INV_N = 1.0 / 2.6;
    if (r > 0.5) {
        if (p.x < r && p.y < r && corners.x > 0.5) {
            vec2 q = max(vec2(r - p.x, r - p.y), vec2(1e-5));
            return pow(pow(q.x, N) + pow(q.y, N), INV_N) - r;
        } else if (p.x > size.x - r && p.y < r && corners.y > 0.5) {
            vec2 q = max(vec2(p.x - (size.x - r), r - p.y), vec2(1e-5));
            return pow(pow(q.x, N) + pow(q.y, N), INV_N) - r;
        } else if (p.x > size.x - r && p.y > size.y - r && corners.z > 0.5) {
            vec2 q = max(vec2(p.x - (size.x - r), p.y - (size.y - r)), vec2(1e-5));
            return pow(pow(q.x, N) + pow(q.y, N), INV_N) - r;
        } else if (p.x < r && p.y > size.y - r && corners.w > 0.5) {
            vec2 q = max(vec2(r - p.x, p.y - (size.y - r)), vec2(1e-5));
            return pow(pow(q.x, N) + pow(q.y, N), INV_N) - r;
        }
    }
    float d_left = -p.x;
    float d_right = p.x - size.x;
    float d_bottom = p.y - size.y;
    float d_top = (corners.x > 0.5 || corners.y > 0.5) ? -p.y : -1e6;
    return max(max(d_left, d_right), max(d_top, d_bottom));
}

vec4 sample_px(vec2 p) {
    vec2 uv = src_off + clamp(p / size, vec2(0.0), vec2(1.0)) * src_scale;
    return texture2D(tex, uv);
}

void main() {
    vec2 p = (v_coords - src_off) / max(src_scale, vec2(1e-5)) * size;
    float sd = box_sdf(p);
    float mask = clamp(0.5 - sd, 0.0, 1.0);

    float bezel = max(glass.x, 1.0);
    float depth = glass.y;
    float spec = glass.z;
    float clear = glass.w;

    // outward surface normal of the slab's edge (gradient of the SDF)
    const float E = 1.0;
    vec2 n = vec2(box_sdf(p + vec2(E, 0.0)) - box_sdf(p - vec2(E, 0.0)),
                  box_sdf(p + vec2(0.0, E)) - box_sdf(p - vec2(0.0, E)));
    float nl = length(n);
    n = nl > 1e-4 ? n / nl : vec2(0.0);

    // 0 in the flat middle, 1 at the very edge; a circular bezel profile
    // makes the bend steepen towards the rim
    float t = clamp(1.0 + sd / bezel, 0.0, 1.0);
    float bend = 1.0 - sqrt(max(1.0 - t * t, 0.0));

    // refraction: look further inward near the edge, splitting the colours
    vec2 off = -n * depth * bend;
    vec4 color;
    color.r = sample_px(p + off * 1.00).r;
    vec4 g = sample_px(p + off * 1.06);
    color.g = g.g;
    color.a = g.a;
    color.b = sample_px(p + off * 1.12).b;

    // a touch of lift so the glass reads as a material, more when frosted
    color.rgb += color.a * (0.015 + 0.05 * (1.0 - clear));

    // specular: light from the top-left reflects off both the near and far
    // side of the bezel (the far rim glows dimmer)
    vec2 L = normalize(vec2(-0.6, -0.8));
    float facing = dot(n, L);
    float lit = max(facing, 0.0) + 0.45 * max(-facing, 0.0);
    float rim_band = smoothstep(-2.2, -0.2, sd);
    float glow_band = t * t;
    float highlight = spec * lit * (0.85 * rim_band + 0.22 * glow_band);
    // a faint dark line just inside the rim gives the edge some thickness
    float edge_shade = 0.10 * spec * smoothstep(-4.0, -2.2, sd) * (1.0 - rim_band);

    color.rgb = color.rgb * (1.0 - edge_shade) + vec3(highlight) * color.a;

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    color = color * mask;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
