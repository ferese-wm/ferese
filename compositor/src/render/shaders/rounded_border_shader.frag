
precision highp float;

uniform float alpha;
uniform vec4 clip_rect;
uniform float radius;
uniform float border_width;
uniform vec4 border_color;
uniform vec4 border_color_to;
uniform vec4 gradient_line;
uniform vec4 focus_color;
uniform vec4 focus_color_to;
uniform vec4 focus_gradient_line;
uniform float focus_mix;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

//_CORNERS_

void main() {
    float signed_distance = rounded_rect_distance(gl_FragCoord.xy, clip_rect, radius);
    float outer_coverage = edge_coverage(signed_distance);

#ifdef CONTINUOUS_WINDOW_CORNERS
    // Inset the outer distance field along its normals, rather than shrinking
    // the profile's bounding box/radius and changing border thickness.
    float inner_coverage = edge_coverage(signed_distance + border_width);
#else
    vec2 inner_size = max(clip_rect.zw - vec2(2.0 * border_width), vec2(0.0));
    vec4 inner_rect = vec4(clip_rect.xy + (clip_rect.zw - inner_size) * 0.5, inner_size);
    float inner_radius = max(radius - border_width, 0.0);
    float inner_coverage = edge_coverage(rounded_rect_distance(gl_FragCoord.xy, inner_rect, inner_radius));
#endif
    float coverage = max(outer_coverage - inner_coverage, 0.0);
    float progress = clamp(dot(gl_FragCoord.xy - gradient_line.xy, gradient_line.zw), 0.0, 1.0);
    // Interpolate premultiplied endpoints: a transparent endpoint must not
    // leak its RGB into the visible border or create a dark halo.
    vec4 from = vec4(border_color.rgb * border_color.a, border_color.a);
    vec4 to = vec4(border_color_to.rgb * border_color_to.a, border_color_to.a);
    float focus_progress = clamp(dot(gl_FragCoord.xy - focus_gradient_line.xy, focus_gradient_line.zw), 0.0, 1.0);
    vec4 focus_from = vec4(focus_color.rgb * focus_color.a, focus_color.a);
    vec4 focus_to = vec4(focus_color_to.rgb * focus_color_to.a, focus_color_to.a);
    vec4 color = mix(mix(from, to, progress), mix(focus_from, focus_to, focus_progress), focus_mix) * coverage * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
