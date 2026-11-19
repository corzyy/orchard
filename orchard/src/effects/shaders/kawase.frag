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

// Dual Kawase blur. `halfpixel` is 0.5 / (source texture size), already
// multiplied by the sample offset. `up` selects the upsample pass.
uniform vec2 halfpixel;
uniform float up;
// Texture coordinates are clamped to this box (min.xy, max.zw): the blur only
// ever sees the pixels below the backdrop, edges are extended.
uniform vec4 clampbox;

vec4 s(vec2 p) {
    return texture2D(tex, clamp(p, clampbox.xy, clampbox.zw));
}

void main() {
    vec2 uv = v_coords;
    vec2 h = halfpixel;
    vec4 sum;
    if (up < 0.5) {
        sum = s(uv) * 4.0;
        sum += s(uv - h);
        sum += s(uv + h);
        sum += s(uv + vec2(h.x, -h.y));
        sum += s(uv - vec2(h.x, -h.y));
        sum = sum / 8.0;
    } else {
        sum = s(uv + vec2(-h.x * 2.0, 0.0));
        sum += s(uv + vec2(-h.x, h.y)) * 2.0;
        sum += s(uv + vec2(0.0, h.y * 2.0));
        sum += s(uv + vec2(h.x, h.y)) * 2.0;
        sum += s(uv + vec2(h.x * 2.0, 0.0));
        sum += s(uv + vec2(h.x, -h.y)) * 2.0;
        sum += s(uv + vec2(0.0, -h.y * 2.0));
        sum += s(uv + vec2(-h.x, -h.y)) * 2.0;
        sum = sum / 12.0;
    }

#if defined(NO_ALPHA)
    sum = vec4(sum.rgb, 1.0);
#endif

    gl_FragColor = sum * alpha;
}
