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
// 1.0 on the final upsample pass: applies vibrancy (saturation boost +
// subtle luminance lift) and anti-banding dither.
uniform float final_pass;
// Texture coordinates are clamped to this box (min.xy, max.zw): the blur only
// ever sees the pixels below the backdrop, edges are extended.
uniform vec4 clampbox;

vec4 s(vec2 p) {
    return texture2D(tex, clamp(p, clampbox.xy, clampbox.zw));
}

float hash12(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
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

    if (final_pass > 0.5 && sum.a > 0.001) {
        vec3 rgb = sum.rgb / sum.a;
        // macOS-style vibrancy: boost saturation and gently lift contrast/luminance
        float luma = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
        rgb = mix(vec3(luma), rgb, 1.45);
        rgb = (rgb - 0.5) * 1.04 + 0.5 + 0.012;
        // subtle triangular dither to eliminate 8-bit banding over smooth gradients
        float d = (hash12(gl_FragCoord.xy) + hash12(gl_FragCoord.xy + vec2(17.0, 31.0)) - 1.0) * (1.0 / 255.0);
        rgb = clamp(rgb + vec3(d), 0.0, 1.0);
        sum.rgb = rgb * sum.a;
    }

    gl_FragColor = sum * alpha;
}
