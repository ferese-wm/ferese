use smithay::reexports::drm::control::crtc;

pub(super) struct Candidates {
    pub applied: Option<crtc::Handle>,
    pub kernel: Option<crtc::Handle>,
    pub compatible: Vec<crtc::Handle>,
}

/// Maximum-cardinality, minimum-cost bipartite matching. Cardinality wins over
/// preferences: augment until no path remains, even if its cost is positive.
/// For that cardinality, retain the most valid applied assignments, then the
/// most kernel assignments. Stable connector/compatible-CRTC order breaks ties.
/// One applied edge outweighs every possible kernel edge, since applied ownership
/// is authoritative and encoder routing can be stale after disabling an output.
pub(super) fn assign(requests: &[Candidates]) -> Vec<Option<crtc::Handle>> {
    let mut crtcs = Vec::new();
    for request in requests {
        for crtc in &request.compatible {
            if !crtcs.contains(crtc) {
                crtcs.push(*crtc);
            }
        }
    }
    let source = requests.len() + crtcs.len();
    let sink = source + 1;
    let mut edges = Vec::new();
    let mut assignments = Vec::new();
    let applied_weight = requests.len() as i64 + 1;
    for (index, request) in requests.iter().enumerate() {
        add_edge(&mut edges, source, index, 0);
        for (crtc_index, crtc) in crtcs.iter().enumerate() {
            if request.compatible.contains(crtc) {
                let cost = -applied_weight * i64::from(request.applied == Some(*crtc))
                    - i64::from(request.kernel == Some(*crtc));
                assignments.push((index, *crtc, edges.len()));
                add_edge(&mut edges, index, requests.len() + crtc_index, cost);
            }
        }
    }
    for crtc_index in 0..crtcs.len() {
        add_edge(&mut edges, requests.len() + crtc_index, sink, 0);
    }
    while augment(&mut edges, source, sink) {}
    let mut assigned = vec![None; requests.len()];
    for (index, crtc, edge) in assignments {
        if !edges[edge].available {
            assigned[index] = Some(crtc);
        }
    }
    assigned
}

struct Edge {
    from: usize,
    to: usize,
    cost: i64,
    available: bool,
}

fn add_edge(edges: &mut Vec<Edge>, from: usize, to: usize, cost: i64) {
    edges.push(Edge {
        from,
        to,
        cost,
        available: true,
    });
    edges.push(Edge {
        from: to,
        to: from,
        cost: -cost,
        available: false,
    });
}

/// Find the cheapest residual augmenting path. Reverse edges allow earlier
/// assignments to move. Bellman-Ford handles negative preference costs; starting
/// from an empty matching and using shortest paths leaves no negative cost cycle.
/// Failed searches leave the matching untouched. Strict improvements and stable
/// edge traversal make equal-cost choices deterministic without hash iteration.
fn augment(edges: &mut [Edge], source: usize, sink: usize) -> bool {
    let mut distances = vec![None; sink + 1];
    let mut previous = vec![None; sink + 1];
    distances[source] = Some(0);
    for _ in 0..sink {
        let mut changed = false;
        for (index, edge) in edges.iter().enumerate() {
            if edge.available
                && let Some(distance) = distances[edge.from]
                && distances[edge.to].is_none_or(|current| distance + edge.cost < current)
            {
                distances[edge.to] = Some(distance + edge.cost);
                previous[edge.to] = Some(index);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    if previous[sink].is_none() {
        return false;
    }
    let mut vertex = sink;
    while vertex != source {
        let index = previous[vertex].expect("reachable augmenting path");
        edges[index].available = false;
        edges[index ^ 1].available = true;
        vertex = edges[index].from;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn crtc(value: u32) -> crtc::Handle {
        std::num::NonZeroU32::new(value).unwrap().into()
    }

    fn candidates(applied: Option<u32>, kernel: Option<u32>, compatible: &[u32]) -> Candidates {
        Candidates {
            applied: applied.map(crtc),
            kernel: kernel.map(crtc),
            compatible: compatible.iter().copied().map(crtc).collect(),
        }
    }

    #[test]
    fn every_connector_order_finds_a_complete_matching() {
        let graph = [&[1, 2][..], &[1, 3][..], &[1, 3][..]];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let requests = order.map(|index| candidates(None, None, graph[index]));
            let result = assign(&requests);
            assert_eq!(result.iter().flatten().count(), 3, "order {order:?}");
            assert_eq!(
                assign(&requests),
                result,
                "deterministic for the same inventory"
            );
        }
    }

    #[test]
    fn equal_preferences_use_connector_order() {
        let result = assign(&[
            candidates(Some(1), None, &[1, 2]),
            candidates(Some(1), None, &[1, 2]),
        ]);
        assert_eq!(result, vec![Some(crtc(1)), Some(crtc(2))]);
    }

    #[test]
    fn preserving_two_applied_outputs_outweighs_preserving_the_first_one() {
        assert_eq!(
            assign(&[
                candidates(Some(1), None, &[1, 4]),
                candidates(Some(2), None, &[2, 4]),
                candidates(Some(3), None, &[2, 3]),
                candidates(None, None, &[1, 3]),
            ]),
            vec![Some(crtc(4)), Some(crtc(2)), Some(crtc(3)), Some(crtc(1))]
        );
    }

    #[test]
    fn maximum_cardinality_and_preference_objective_match_exhaustive_oracle() {
        fn score(
            requests: &[Candidates],
            assignment: &[Option<crtc::Handle>],
        ) -> (usize, usize, usize) {
            let retained = [true, false].map(|applied| {
                requests
                    .iter()
                    .zip(assignment)
                    .filter(|(request, actual)| {
                        let preferred = if applied {
                            request.applied
                        } else {
                            request.kernel
                        };
                        preferred.is_some() && preferred == **actual
                    })
                    .count()
            });
            (
                assignment.iter().flatten().count(),
                retained[0],
                retained[1],
            )
        }

        fn enumerate(
            requests: &[Candidates],
            prefix: &mut Vec<Option<crtc::Handle>>,
            best: &mut (usize, usize, usize),
        ) {
            if prefix.len() == requests.len() {
                *best = std::cmp::max(*best, score(requests, prefix));
                return;
            }
            let index = prefix.len();
            prefix.push(None);
            enumerate(requests, prefix, best);
            prefix.pop();
            for crtc in &requests[index].compatible {
                if !prefix.contains(&Some(*crtc)) {
                    prefix.push(Some(*crtc));
                    enumerate(requests, prefix, best);
                    prefix.pop();
                }
            }
        }
        // All 512 three-by-three graphs, including empty and impossible ones.
        // Vary both preference classes, including invalid/conflicting preferences.
        for graph in 0..512 {
            for seed in 0..8 {
                let requests = (0..3)
                    .map(|index| {
                        let compatible = (1..=3)
                            .filter(|crtc| graph & (1 << (index * 3 + crtc - 1)) != 0)
                            .collect::<Vec<_>>();
                        let applied = ((seed + index) % 4 != 0).then_some((seed + index) % 4);
                        let kernel =
                            ((seed * 3 + index) % 4 != 0).then_some((seed * 3 + index) % 4);
                        candidates(applied, kernel, &compatible)
                    })
                    .collect::<Vec<_>>();
                let mut best = (0, 0, 0);
                enumerate(&requests, &mut Vec::new(), &mut best);
                let actual = assign(&requests);
                let used = actual.iter().flatten().collect::<HashSet<_>>();
                assert_eq!(used.len(), actual.iter().flatten().count());
                assert!(requests.iter().zip(&actual).all(|(request, crtc)| {
                    crtc.is_none_or(|crtc| request.compatible.contains(&crtc))
                }));
                assert_eq!(
                    score(&requests, &actual),
                    best,
                    "graph {graph}, preferences {seed}"
                );
            }
        }
    }

    #[test]
    fn three_flexible_connectors_need_an_augmenting_path() {
        let result = assign(&[
            candidates(None, None, &[1, 2]),
            candidates(None, None, &[1, 3]),
            candidates(None, None, &[1, 3]),
        ]);
        assert_eq!(result.iter().flatten().count(), 3);
    }

    #[test]
    fn completeness_can_require_moving_two_applied_outputs() {
        let result = assign(&[
            candidates(Some(1), Some(1), &[1, 2]),
            candidates(Some(2), Some(2), &[2, 3]),
            candidates(None, None, &[1]),
        ]);
        assert_eq!(result, vec![Some(crtc(2)), Some(crtc(3)), Some(crtc(1))]);
    }

    #[test]
    fn external_to_two_outputs_keeps_external_crtc_regardless_of_connector_order() {
        // Extend and mirror both require two independent scanout assignments.
        // The internal panel appears first in the DRM connector inventory.
        let internal = candidates(None, None, &[151, 270]);
        let external = candidates(Some(151), Some(151), &[151, 270]);
        assert_eq!(
            assign(&[internal, external]),
            vec![Some(crtc(270)), Some(crtc(151))]
        );
        let internal = candidates(None, None, &[151, 270]);
        let external = candidates(Some(151), Some(151), &[151, 270]);
        assert_eq!(
            assign(&[external, internal]),
            vec![Some(crtc(151)), Some(crtc(270))]
        );
    }

    #[test]
    fn stale_disabled_encoder_cannot_steal_applied_external_assignment() {
        assert_eq!(
            assign(&[
                candidates(None, Some(151), &[151, 270]),
                candidates(Some(151), Some(151), &[151, 270]),
            ]),
            vec![Some(crtc(270)), Some(crtc(151))]
        );
    }

    #[test]
    fn kernel_assignments_are_preserved_when_no_outputs_have_been_applied() {
        assert_eq!(
            assign(&[
                candidates(None, None, &[151, 270]),
                candidates(None, Some(151), &[151, 270]),
            ]),
            vec![Some(crtc(270)), Some(crtc(151))]
        );
    }

    #[test]
    fn returning_to_one_output_and_reconnecting_preserves_surviving_assignment() {
        assert_eq!(
            assign(&[candidates(Some(270), Some(270), &[151, 270])]),
            vec![Some(crtc(270))]
        );
        assert_eq!(
            assign(&[
                candidates(None, None, &[151, 270]),
                candidates(Some(270), Some(270), &[151, 270]),
            ]),
            vec![Some(crtc(151)), Some(crtc(270))]
        );
    }

    #[test]
    fn incompatible_previous_assignment_is_not_reserved() {
        assert_eq!(
            assign(&[candidates(Some(151), Some(151), &[270])]),
            vec![Some(crtc(270))]
        );
    }

    #[test]
    fn restricted_connector_gets_its_only_free_crtc() {
        assert_eq!(
            assign(&[
                candidates(None, None, &[151, 270]),
                candidates(None, None, &[151])
            ]),
            vec![Some(crtc(270)), Some(crtc(151))]
        );
    }

    #[test]
    fn exhausted_crtcs_do_not_displace_surviving_output_or_assign_twice() {
        assert_eq!(
            assign(&[
                candidates(None, None, &[151]),
                candidates(Some(151), Some(151), &[151])
            ]),
            vec![None, Some(crtc(151))]
        );
        assert!(assign(&[]).is_empty());
        assert_eq!(assign(&[candidates(None, None, &[])]), vec![None]);
    }
}
