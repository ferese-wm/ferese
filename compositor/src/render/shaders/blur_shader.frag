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
uniform float presentation_alpha;
uniform float background_opacity;
uniform vec4 tint;
varying vec2 v_coords;
//_CORNERS_
//_MATERIAL_CORNERS_
//_PANEL_MASK_

void main() {
    float sdf = material_distance(gl_FragCoord.xy, visible_rect, material_radii);
    // The capture includes a sampling halo, but this draw owns only its visible
    // region. The shared panel mask also contains neighboring islands.
    if (panel_count > 0.5) sdf = max(sdf, panel_box_distance(gl_FragCoord.xy, visible_rect));
    float coverage = alpha * presentation_alpha * edge_coverage(sdf);
    if (coverage <= 0.0) { gl_FragColor = vec4(0.0); return; }
    vec2 coords = (gl_FragCoord.xy - capture_origin) / texture_size;
    vec3 background = texture2D(tex, coords).rgb;
    gl_FragColor = vec4(mix(background, tint.rgb, tint.a * background_opacity) * coverage, coverage);
}
