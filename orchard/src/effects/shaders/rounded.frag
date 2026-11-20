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

float corner_alpha(vec2 p) {
    vec2 c;
    float r = radius;
    if (p.x < r && p.y < r && corners.x > 0.5) {
        c = vec2(r, r);
    } else if (p.x > size.x - r && p.y < r && corners.y > 0.5) {
        c = vec2(size.x - r, r);
    } else if (p.x > size.x - r && p.y > size.y - r && corners.z > 0.5) {
        c = vec2(size.x - r, size.y - r);
    } else if (p.x < r && p.y > size.y - r && corners.w > 0.5) {
        c = vec2(r, size.y - r);
    } else {
        return 1.0;
    }
    float d = distance(p, c);
    return clamp(r - d + 0.5, 0.0, 1.0);
}

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    color = color * corner_alpha((v_coords - src_off) / max(src_scale, vec2(1e-5)) * size);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
