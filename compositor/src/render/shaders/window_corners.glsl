// Apple-style corner approximation: three cubic Beziers per corner, using
// measured UIKit path coordinates (radius units), rather than a superellipse.
// Source and limits: https://liamrosenfeld.com/posts/apple_icon_quest/
// CoreGraphics' private implementation is not available on Linux. These
// published cubics have small tangent/curvature jumps at their internal joins;
// they must not be described as an exact globally G1/G2-continuous curve.
const float CORNER_EXTENT = 1.52866498;

void corner_controls(int segment, float blend, out vec2 a, out vec2 b, out vec2 c, out vec2 d) {
    if (segment == 0) {
        a = mix(vec2(0.0, 1.0), vec2(0.0, CORNER_EXTENT), blend);
        b = mix(vec2(0.0, 0.8686781289), vec2(0.0, 1.08849296), blend);
        c = mix(vec2(0.0258657631, 0.7386421566), vec2(0.0, 0.86840694), blend);
        d = mix(vec2(0.0761204675, 0.6173165676), vec2(0.07491139, 0.63149379), blend);
    } else if (segment == 1) {
        a = mix(vec2(0.0761204675, 0.6173165676), vec2(0.07491139, 0.63149379), blend);
        b = mix(vec2(0.1776144241, 0.3722884810), vec2(0.16905956, 0.37282383), blend);
        c = b.yx;
        d = a.yx;
    } else {
        a = mix(vec2(0.6173165676, 0.0761204675), vec2(0.63149379, 0.07491139), blend);
        b = mix(vec2(0.7386421566, 0.0258657631), vec2(0.86840694, 0.0), blend);
        c = mix(vec2(0.8686781289, 0.0), vec2(1.08849296, 0.0), blend);
        d = mix(vec2(1.0, 0.0), vec2(CORNER_EXTENT, 0.0), blend);
    }
}

void corner_sample(float t, vec2 a, vec2 b, vec2 c, vec2 d,
                   out vec2 point, out vec2 tangent, out vec2 acceleration) {
    vec2 first = 3.0 * (b - a);
    vec2 second = 3.0 * (c - 2.0 * b + a);
    vec2 third = d - 3.0 * c + 3.0 * b - a;
    point = a + t * (first + t * (second + t * third));
    tangent = first + t * (2.0 * second + 3.0 * t * third);
    acceleration = 2.0 * second + 6.0 * t * third;
}

float corner_nearest(vec2 query, float t, vec2 a, vec2 b, vec2 c, vec2 d) {
    // Bounded safeguarded Newton steps. Both endpoint seeds are needed for
    // interior points with two minima; the midpoint may be a local maximum.
    for (int iteration = 0; iteration < 8; iteration++) {
        vec2 point, tangent, acceleration;
        corner_sample(t, a, b, c, d, point, tangent, acceleration);
        vec2 error = point - query;
        float speed_squared = dot(tangent, tangent);
        float denominator = max(speed_squared + dot(error, acceleration), speed_squared * 0.25);
        float next = clamp(t - clamp(dot(error, tangent) / denominator, -0.25, 0.25), 0.0, 1.0);
        if (next == t) {
            break;
        }
        t = next;
    }

    return t;
}

float rounded_rect_distance(vec2 point, vec4 rect, float radius) {
    vec2 half_size = rect.zw * 0.5;
    vec2 inset = half_size - abs(point - rect.xy - half_size);
    float limit = min(half_size.x, half_size.y);

    // Preserve exact circular pills/circles and square/fullscreen outlines.
    if (radius <= 0.0 || radius >= limit) {
        vec2 q = vec2(radius) - inset;
        return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
    }

    // The measured profile extends about 1.53 radii along the straight edge.
    // When that would overlap its opposite corner, smoothly converge to the
    // circular profile. This size adaptation is Ferese policy, not UIKit code.
    float blend = clamp((limit / radius - 1.0) / (CORNER_EXTENT - 1.0), 0.0, 1.0);
    float extent = radius * mix(1.0, CORNER_EXTENT, blend);

    if (max(inset.x, inset.y) >= extent) {
        return -min(inset.x, inset.y);
    }

    // Reflect onto the first half of the symmetric corner. The opposite
    // shoulder cannot be nearer than its reflection, so omit segment 2.
    vec2 query = vec2(min(inset.x, inset.y), max(inset.x, inset.y)) / radius;
    float best_squared = 1.0e20;
    float side = 1.0;

    for (int segment = 0; segment < 2; segment++) {
        vec2 a, b, c, d;
        corner_controls(segment, blend, a, b, c, d);
        // The cubic stays inside its control hull. Reject only when even
        // that hull's box cannot improve the closest point found so far.
        vec2 low = min(min(a, b), min(c, d));
        vec2 high = max(max(a, b), max(c, d));
        vec2 delta = max(max(low - query, query - high), 0.0);
        if (dot(delta, delta) > best_squared) {
            continue;
        }

        for (int seed = 0; seed < 2; seed++) {
            float t = corner_nearest(query, float(seed), a, b, c, d);
            vec2 closest, tangent, acceleration;
            corner_sample(t, a, b, c, d, closest, tangent, acceleration);
            vec2 error = query - closest;
            float squared = dot(error, error);

            if (squared < best_squared) {
                best_squared = squared;
                side = dot(error, vec2(tangent.y, -tangent.x));
            }
        }
    }

    return sqrt(best_squared) * radius * (side < 0.0 ? -1.0 : 1.0);
}

float edge_coverage(float distance) {
    return clamp(0.5 - distance, 0.0, 1.0);
}
