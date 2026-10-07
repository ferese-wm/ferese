use std::collections::HashSet;

use smithay::reexports::drm::control::crtc;

pub(super) struct Candidates {
    pub applied: Option<crtc::Handle>,
    pub kernel: Option<crtc::Handle>,
    pub compatible: Vec<crtc::Handle>,
}

/// Reserve surviving outputs before assigning newly enabled connectors. Kernel
/// encoder routing can be stale for a disabled connector; applied ownership wins.
pub(super) fn assign(requests: &[Candidates]) -> Vec<Option<crtc::Handle>> {
    let mut assigned = vec![None; requests.len()];
    let mut used = HashSet::new();

    for applied in [true, false] {
        for (request, assigned) in requests.iter().zip(&mut assigned) {
            let preferred = if applied { request.applied } else { request.kernel };

            if assigned.is_none()
                && let Some(crtc) = preferred.filter(|crtc| request.compatible.contains(crtc))
                && used.insert(crtc)
            {
                *assigned = Some(crtc);
            }
        }
    }

    // Give connectors with fewer choices the first free CRTC.
    let mut remaining = (0..requests.len())
        .filter(|index| assigned[*index].is_none())
        .collect::<Vec<_>>();

    remaining.sort_by_key(|index| requests[*index].compatible.len());

    for index in remaining {
        if let Some(crtc) = requests[index].compatible.iter().find(|crtc| !used.contains(crtc)) {
            used.insert(*crtc);
            assigned[index] = Some(*crtc);
        }
    }

    assigned
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn external_to_two_outputs_keeps_external_crtc_regardless_of_connector_order() {
        // Extend and mirror both require two independent scanout assignments.
        // The internal panel appears first in the DRM connector inventory.
        let internal = candidates(None, None, &[151, 270]);
        let external = candidates(Some(151), Some(151), &[151, 270]);
        assert_eq!(assign(&[internal, external]), vec![Some(crtc(270)), Some(crtc(151))]);
        let internal = candidates(None, None, &[151, 270]);
        let external = candidates(Some(151), Some(151), &[151, 270]);
        assert_eq!(assign(&[external, internal]), vec![Some(crtc(151)), Some(crtc(270))]);
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
            assign(&[candidates(None, None, &[151, 270]), candidates(None, None, &[151])]),
            vec![Some(crtc(270)), Some(crtc(151))]
        );
    }

    #[test]
    fn exhausted_crtcs_do_not_displace_surviving_output_or_assign_twice() {
        assert_eq!(
            assign(&[candidates(None, None, &[151]), candidates(Some(151), Some(151), &[151])]),
            vec![None, Some(crtc(151))]
        );
        assert!(assign(&[]).is_empty());
        assert_eq!(assign(&[candidates(None, None, &[])]), vec![None]);
    }
}
