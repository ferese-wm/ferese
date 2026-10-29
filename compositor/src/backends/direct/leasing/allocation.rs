//! Reserve connectors, CRTCs and primary planes together. A maximum-flow graph
//! avoids greedy assignments that strand a connector behind a shared plane.

use smithay::reexports::drm::control::{connector, crtc, plane};
use std::collections::VecDeque;

pub(super) struct Request {
    pub connector: connector::Handle,
    pub crtcs: Vec<crtc::Handle>,
}

pub(super) struct PrimaryPlanes {
    pub crtc: crtc::Handle,
    pub planes: Vec<plane::Handle>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Allocation {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub primary: plane::Handle,
}

struct Edge {
    destination: usize,
    reverse: usize,
    available: bool,
}

fn connect(graph: &mut [Vec<Edge>], from: usize, to: usize) -> usize {
    let forward = graph[from].len();
    let reverse = graph[to].len();
    graph[from].push(Edge {
        destination: to,
        reverse,
        available: true,
    });
    graph[to].push(Edge {
        destination: from,
        reverse: forward,
        available: false,
    });
    forward
}

fn augment(graph: &mut [Vec<Edge>], source: usize, sink: usize) -> bool {
    let mut path = vec![None; graph.len()];
    let mut queue = VecDeque::from([source]);
    path[source] = Some((source, 0));
    while let Some(from) = queue.pop_front() {
        let edge_iter = graph[from].iter().enumerate();
        for (edge_index, edge) in edge_iter {
            if edge.available && path[edge.destination].is_none() {
                path[edge.destination] = Some((from, edge_index));
                queue.push_back(edge.destination);
            }
        }
        if path[sink].is_some() {
            break;
        }
    }
    if path[sink].is_none() {
        return false;
    }
    let mut to = sink;
    while to != source {
        let (from, edge_index) = path[to].expect("sink is reachable through residual edges");
        let reverse = graph[from][edge_index].reverse;
        graph[from][edge_index].available = false;
        graph[to][reverse].available = true;
        to = from;
    }
    true
}

pub(super) fn allocate(
    requests: &[Request],
    primary_planes: &[PrimaryPlanes],
) -> Option<Vec<Allocation>> {
    if requests.is_empty() {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    if requests
        .iter()
        .any(|request| !seen.insert(request.connector))
    {
        return None;
    }
    let mut crtcs = primary_planes
        .iter()
        .map(|entry| entry.crtc)
        .collect::<Vec<_>>();
    crtcs.sort_by_key(|crtc| u32::from(*crtc));
    crtcs.dedup();
    let mut planes = primary_planes
        .iter()
        .flat_map(|entry| entry.planes.iter().copied())
        .collect::<Vec<_>>();
    planes.sort_by_key(|plane| u32::from(*plane));
    planes.dedup();
    let crtc_start = requests.len();
    let plane_start = crtc_start + 2 * crtcs.len();
    let source = plane_start + planes.len();
    let sink = source + 1;
    let mut graph = (0..=sink).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut connector_edges = Vec::new();
    let mut plane_edges = Vec::new();
    let request_iter = requests.iter().enumerate();
    for (index, request) in request_iter {
        connect(&mut graph, source, index);
        let crtc_iter = crtcs.iter().enumerate();
        for (crtc_index, crtc) in crtc_iter {
            if request.crtcs.contains(crtc) {
                let edge = connect(&mut graph, index, crtc_start + 2 * crtc_index);
                connector_edges.push((index, *crtc, edge));
            }
        }
    }
    let crtc_iter = crtcs.iter().enumerate();
    for (index, crtc) in crtc_iter {
        let input = crtc_start + 2 * index;
        let output = input + 1;
        connect(&mut graph, input, output);
        let plane_iter = planes.iter().enumerate();
        for (plane_index, plane) in plane_iter {
            if primary_planes
                .iter()
                .any(|entry| entry.crtc == *crtc && entry.planes.contains(plane))
            {
                let edge = connect(&mut graph, output, plane_start + plane_index);
                plane_edges.push((*crtc, *plane, output, edge));
            }
        }
    }
    for index in 0..planes.len() {
        connect(&mut graph, plane_start + index, sink);
    }
    for _ in 0..requests.len() {
        if !augment(&mut graph, source, sink) {
            return None;
        }
    }
    connector_edges
        .into_iter()
        .filter(|(index, _, edge)| !graph[*index][*edge].available)
        .map(|(index, crtc, _)| {
            let (_, primary, _, _) = plane_edges.iter().find(|(candidate, _, node, edge)| {
                *candidate == crtc && !graph[*node][*edge].available
            })?;
            Some(Allocation {
                connector: requests[index].connector,
                crtc,
                primary: *primary,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    fn crtc(value: u32) -> crtc::Handle {
        NonZeroU32::new(value).unwrap().into()
    }
    fn plane(value: u32) -> plane::Handle {
        NonZeroU32::new(value).unwrap().into()
    }
    fn request(value: u32, crtcs: &[u32]) -> Request {
        Request {
            connector: NonZeroU32::new(value).unwrap().into(),
            crtcs: crtcs.iter().copied().map(crtc).collect(),
        }
    }
    fn primary(value: u32, planes: &[u32]) -> PrimaryPlanes {
        PrimaryPlanes {
            crtc: crtc(value),
            planes: planes.iter().copied().map(plane).collect(),
        }
    }

    #[test]
    fn matching_reroutes_connectors_and_planes_instead_of_stranding_later_heads() {
        let requests = [request(1, &[1, 2]), request(2, &[1])];
        let result = allocate(&requests, &[primary(1, &[1]), primary(2, &[1, 2])]).unwrap();
        assert_eq!(result[0].crtc, crtc(2));
        assert_eq!(result[0].primary, plane(2));
        assert_eq!(result[1].crtc, crtc(1));
        assert_eq!(result[1].primary, plane(1));
    }

    #[test]
    fn duplicate_empty_unroutable_or_shared_resource_requests_fail_atomically() {
        assert!(allocate(&[], &[]).is_none());
        assert!(
            allocate(
                &[request(1, &[1]), request(1, &[2])],
                &[primary(1, &[1]), primary(2, &[2])]
            )
            .is_none()
        );
        assert!(allocate(&[request(1, &[])], &[primary(1, &[1])]).is_none());
        assert!(allocate(&[request(1, &[1]), request(2, &[1])], &[primary(1, &[1])]).is_none());
        assert!(
            allocate(
                &[request(1, &[1]), request(2, &[2])],
                &[primary(1, &[1]), primary(2, &[1])]
            )
            .is_none()
        );
        assert!(allocate(&[request(1, &[1])], &[primary(1, &[])]).is_none());
    }

    #[test]
    fn every_connector_order_keeps_resources_exclusive() {
        for order in [
            [1, 2, 3],
            [1, 3, 2],
            [2, 1, 3],
            [2, 3, 1],
            [3, 1, 2],
            [3, 2, 1],
        ] {
            let requests =
                order.map(|id| request(id, if id == 1 { &[1, 2][..] } else { &[1, 3][..] }));
            let result = allocate(
                &requests,
                &[
                    primary(1, &[1, 2]),
                    primary(2, &[1, 3]),
                    primary(3, &[2, 3]),
                ],
            )
            .unwrap();
            assert_eq!(result.len(), 3);
            assert_eq!(
                result
                    .iter()
                    .map(|allocation| allocation.crtc)
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                3
            );
            assert_eq!(
                result
                    .iter()
                    .map(|allocation| allocation.primary)
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                3
            );
        }
    }

    #[test]
    fn a_long_alternating_chain_is_solved_without_recursion_or_greedy_fallback() {
        let mut requests = (1..129)
            .map(|id| request(id, &[id, id + 1]))
            .collect::<Vec<_>>();
        requests.push(request(129, &[1]));
        let primary = (1..=129).map(|id| primary(id, &[id])).collect::<Vec<_>>();
        let allocations = allocate(&requests, &primary).unwrap();
        assert_eq!(allocations.len(), 129);
        assert_eq!(allocations.last().unwrap().crtc, crtc(1));
        assert_eq!(
            allocations
                .iter()
                .map(|allocation| allocation.primary)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            129
        );
    }
}
