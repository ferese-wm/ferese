
precision highp float;
uniform float alpha;
uniform vec4 tint;
uniform vec4 visible_rect;
uniform float material_radius;
uniform float paint_mode;
uniform vec4 shadow_rect;
uniform vec2 shadow_values;
varying vec2 v_coords;

//_CORNERS_

void main() {
    float sdf = rounded_rect_distance(gl_FragCoord.xy, visible_rect, material_radius);
    float coverage = edge_coverage(sdf);
    if (paint_mode > 0.5) {
        // The shadow pass is multiplied by (1 - coverage). Fully covered
        // pixels need neither a second contour solve nor a Gaussian sample.
        if (coverage >= 1.0) { gl_FragColor = vec4(0.0); return; }
        float distance = max(rounded_rect_distance(gl_FragCoord.xy, shadow_rect, material_radius), 0.0);
        float sigma = max(shadow_values.x * 0.5, 0.5);
        float opacity = exp(-0.5 * distance * distance / (sigma * sigma))
            * shadow_values.y * (1.0 - coverage) * alpha;
        gl_FragColor = vec4(0.0, 0.0, 0.0, opacity);
    } else {
        float opacity = tint.a * coverage * alpha;
        gl_FragColor = vec4(tint.rgb * opacity, opacity);
    }
}
