// The same signed-distance mask is used by tint, blur and shadow. Shapes are
// bounded surface-local geometry transformed into framebuffer coordinates.
uniform float panel_count;
uniform float panel_fusion;
uniform mat3 panel_shapes[32];

float panel_box_distance(vec2 p, vec4 b) {
    vec2 q = abs(p - b.xy - b.zw * 0.5) - b.zw * 0.5;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0);
}
float panel_round_distance(vec2 p, vec4 b, vec4 r) {
    vec2 q = p - b.xy;
    float d = panel_box_distance(p, b);
    vec2 a = q;
    if (r.x > 0.0 && a.x < r.x && a.y < r.x) d = max(d, length(a - r.x) - r.x);
    a = vec2(b.z - q.x, q.y);
    if (r.y > 0.0 && a.x < r.y && a.y < r.y) d = max(d, length(a - r.y) - r.y);
    a = b.zw - q;
    if (r.z > 0.0 && a.x < r.z && a.y < r.z) d = max(d, length(a - r.z) - r.z);
    a = vec2(q.x, b.w - q.y);
    if (r.w > 0.0 && a.x < r.w && a.y < r.w) d = max(d, length(a - r.w) - r.w);
    return d;
}
float panel_smin(float a, float b) {
    if (panel_fusion <= 0.0) return min(a, b);
    float h = max(panel_fusion - abs(a - b), 0.0) / panel_fusion;
    return min(a, b) - h * h * panel_fusion * 0.25;
}
float panel_distance(vec2 p) {
    vec4 unions = vec4(1.0e8);
    float cutout = 1.0e8;
    for (int i = 0; i < 32; i++) {
        if (float(i) >= panel_count) break;
        mat3 m = panel_shapes[i];
        vec4 b = vec4(m[0].xyz, m[1].x);
        vec4 r = vec4(m[1].yz, m[2].xy);
        float op = floor(m[2].z / 4.0);
        float zone = mod(m[2].z, 4.0);
        float d = panel_round_distance(p, b, r);
        if (op > 1.5) {
            vec2 c = b.xy;
            if (op < 2.5) c.y += b.w;
            else if (op < 3.5) c += b.zw;
            else if (op < 4.5) c.x += b.z;
            d = max(panel_box_distance(p, b), min(b.z, b.w) - length(p - c));
        }
        if (op > 0.5 && op < 1.5) cutout = min(cutout, d);
        else if (zone < 0.5) unions.x = min(unions.x, d);
        else if (zone < 1.5) unions.y = panel_smin(unions.y, d);
        else if (zone < 2.5) unions.z = panel_smin(unions.z, d);
        else unions.w = panel_smin(unions.w, d);
    }
    return max(min(min(unions.x, unions.y), min(unions.z, unions.w)), -cutout);
}
float material_distance(vec2 p, vec4 b, vec4 r) {
    if (panel_count > 0.5) return panel_distance(p);
    return rounded_rect_corners_distance(p, b, r);
}
