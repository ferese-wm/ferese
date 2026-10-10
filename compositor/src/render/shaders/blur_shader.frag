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
uniform vec4 visible_rect;
uniform vec4 material_radii;
uniform vec2 texture_size;
uniform vec2 capture_origin;
uniform float blur_radius;
uniform float presentation_alpha;
uniform float background_opacity;
uniform vec4 tint;
varying vec2 v_coords;
//_CORNERS_
//_MATERIAL_CORNERS_

void main() {
    float sdf = rounded_rect_corners_distance(gl_FragCoord.xy, visible_rect, material_radii);
    float coverage = alpha * presentation_alpha * edge_coverage(sdf);
    if (coverage <= 0.0) { gl_FragColor = vec4(0.0); return; }
    vec2 coords = (gl_FragCoord.xy - capture_origin) / texture_size;
    vec4 color = vec4(0.0);
    float weights = 0.0;
    // A spiral avoids aligning the sampling lattice with wallpaper patterns.
    for (int i = 0; i < 256; i++) {
        float radius = sqrt((float(i) + 0.5) / 256.0);
        float angle = float(i) * 2.39996323;
        vec2 offset = vec2(cos(angle), sin(angle)) * radius * blur_radius / texture_size;
        float weight = exp(-4.5 * radius * radius);
        color += texture2D(tex, coords + offset) * weight;
        weights += weight;
    }
    vec3 background = color.rgb / weights;
    gl_FragColor = vec4(mix(background, tint.rgb, tint.a * background_opacity) * coverage, coverage);
}
