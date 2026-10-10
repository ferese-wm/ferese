// Radii are clockwise from the framebuffer rectangle's top-left.
float rounded_rect_corners_distance(vec2 point, vec4 rect, vec4 radii) {
    vec2 center = rect.xy + rect.zw * 0.5;
    float radius = point.y < center.y
        ? (point.x < center.x ? radii.x : radii.y)
        : (point.x < center.x ? radii.w : radii.z);
    return rounded_rect_distance(point, rect, radius);
}
