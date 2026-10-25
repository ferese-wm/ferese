#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
uniform vec2 cache_size;
uniform vec2 sample_step;
varying vec2 v_coords;

void main() {
    vec2 coords = gl_FragCoord.xy / cache_size;
    // Gaussian sigma 4, truncated at three sigma. Keep individual taps:
    // fractional radii do not place neighboring samples one texel apart.
    vec3 color = vec3(0.0);
    color += texture2D(tex, coords + sample_step * -12.0).rgb * 0.0011098816;
    color += texture2D(tex, coords + sample_step * -11.0).rgb * 0.0022773292;
    color += texture2D(tex, coords + sample_step * -10.0).rgb * 0.0043896669;
    color += texture2D(tex, coords + sample_step * -9.0).rgb * 0.0079486599;
    color += texture2D(tex, coords + sample_step * -8.0).rgb * 0.0135211259;
    color += texture2D(tex, coords + sample_step * -7.0).rgb * 0.0216066977;
    color += texture2D(tex, coords + sample_step * -6.0).rgb * 0.0324354950;
    color += texture2D(tex, coords + sample_step * -5.0).rgb * 0.0457413795;
    color += texture2D(tex, coords + sample_step * -4.0).rgb * 0.0605974824;
    color += texture2D(tex, coords + sample_step * -3.0).rgb * 0.0754147853;
    color += texture2D(tex, coords + sample_step * -2.0).rgb * 0.0881688166;
    color += texture2D(tex, coords + sample_step * -1.0).rgb * 0.0968345011;
    color += texture2D(tex, coords + sample_step * 0.0).rgb * 0.0999083581;
    color += texture2D(tex, coords + sample_step * 1.0).rgb * 0.0968345011;
    color += texture2D(tex, coords + sample_step * 2.0).rgb * 0.0881688166;
    color += texture2D(tex, coords + sample_step * 3.0).rgb * 0.0754147853;
    color += texture2D(tex, coords + sample_step * 4.0).rgb * 0.0605974824;
    color += texture2D(tex, coords + sample_step * 5.0).rgb * 0.0457413795;
    color += texture2D(tex, coords + sample_step * 6.0).rgb * 0.0324354950;
    color += texture2D(tex, coords + sample_step * 7.0).rgb * 0.0216066977;
    color += texture2D(tex, coords + sample_step * 8.0).rgb * 0.0135211259;
    color += texture2D(tex, coords + sample_step * 9.0).rgb * 0.0079486599;
    color += texture2D(tex, coords + sample_step * 10.0).rgb * 0.0043896669;
    color += texture2D(tex, coords + sample_step * 11.0).rgb * 0.0022773292;
    color += texture2D(tex, coords + sample_step * 12.0).rgb * 0.0011098816;
    gl_FragColor = vec4(color, 1.0);
}
