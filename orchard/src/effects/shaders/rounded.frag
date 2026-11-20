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

// size of the textured element in physical pixels
uniform vec2 size;
// corner radius in physical pixels
uniform float radius;
// which corners to round: (top-left, top-right, bottom-right, bottom-left), 1.0 = round
uniform vec4 corners;
// crop of the texture this element samples (normalized)
uniform vec2 src_off;
uniform vec2 src_scale;
// optional inner 1px rim highlight: (r, g, b, alpha); alpha <= 0 disables it
uniform vec4 rim;
// rim thickness in physical pixels
uniform float rim_width;

// Signed distance in physical pixels to a continuous (squircle) rounded box.
// Negative inside, 0 on the edge, positive outside.
float box_sdf(vec2 p) {
    float r = min(radius, min(size.x, size.y) * 0.5);
    // Superellipse exponent for smooth macOS-style continuous corners (n = 3.2)
    const float N = 3.2;
    const float INV_N = 1.0 / 3.2;
    if (r > 0.5) {
        if (p.x < r && p.y < r && corners.x > 0.5) {
            vec2 q = vec2(r - p.x, r - p.y);
            return pow(pow(q.x, N) + pow(q.y, N), INV_N) - r;
        } else if (p.x > size.x - r && p.y < r && corners.y > 0.5) {
            vec2 q = vec2(p.x - (size.x - r), r - p.y);
            return pow(pow(q.x, N) + pow(q.y, N), INV_N) - r;
        } else if (p.x > size.x - r && p.y > size.y - r && corners.z > 0.5) {
            vec2 q = vec2(p.x - (size.x - r), p.y - (size.y - r));
            return pow(pow(q.x, N) + pow(q.y, N), INV_N) - r;
        } else if (p.x < r && p.y > size.y - r && corners.w > 0.5) {
            vec2 q = vec2(r - p.x, p.y - (size.y - r));
            return pow(pow(q.x, N) + pow(q.y, N), INV_N) - r;
        }
    }
    // Straight edge distance (skip top edge when top corners are unrounded,
    // i.e. SSD client surface meeting the titlebar)
    float d_left = -p.x;
    float d_right = p.x - size.x;
    float d_bottom = p.y - size.y;
    float d_top = (corners.x > 0.5 || corners.y > 0.5) ? -p.y : -1e6;
    return max(max(d_left, d_right), max(d_top, d_bottom));
}

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    vec2 p = (v_coords - src_off) / max(src_scale, vec2(1e-5)) * size;
    float sd = box_sdf(p);
    float mask = clamp(0.5 - sd, 0.0, 1.0);

    // Optional crisp inner hairline rim highlight along the rounded contour
    if (rim.a > 0.001) {
        float w = max(rim_width, 1.0);
        float inner = smoothstep(-w - 0.5, -w + 0.5, sd);
        float rim_a = rim.a * inner * alpha;
        // Standard premultiplied "source-over" composite of rim over color
        color = vec4(rim.rgb * rim_a, rim_a) + color * (1.0 - rim_a);
    }

    color = color * mask;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
