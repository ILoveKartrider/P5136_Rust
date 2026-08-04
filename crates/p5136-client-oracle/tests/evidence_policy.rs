use std::collections::HashSet;

use p5136_client_oracle::evidence::{AUDITED, Confidence, FSM_AUDITED};

#[test]
fn evidence_manifest_is_explicit_unique_and_does_not_overclaim() {
    let mut packets = HashSet::new();
    let mut hashes = HashSet::new();
    for evidence in AUDITED {
        assert!(packets.insert(evidence.packet), "duplicate evidence row");
        assert_ne!(evidence.hash, 0);
        assert!(hashes.insert(evidence.hash), "duplicate packet hash");
        assert!(!evidence.source_anchor.is_empty());
        assert!(!evidence.artifact.is_empty());
    }

    let native_layout_exact = AUDITED
        .iter()
        .filter(|evidence| {
            matches!(
                evidence.confidence,
                Confidence::IdbLayoutExactPartialSemantics | Confidence::IdbCodecAndConsumerExact
            )
        })
        .map(|evidence| evidence.packet)
        .collect::<Vec<_>>();
    assert_eq!(
        native_layout_exact,
        [
            "GameSlotPacket type-12 item consumers (63-class expansion)",
            "GameResultPacket",
            "GameNextStagePacket",
        ]
    );
}

#[test]
fn fsm_evidence_is_separate_explicit_and_unique() {
    let mut transitions = HashSet::new();
    for evidence in FSM_AUDITED {
        assert!(transitions.insert(evidence.transition));
        assert!(!evidence.source_anchor.is_empty());
        assert!(!evidence.artifact.is_empty());
    }
    assert_eq!(transitions.len(), 9);
}

#[test]
fn production_oracle_cannot_gain_a_normal_core_dependency_silently() {
    let manifest = include_str!("../Cargo.toml");
    let mut in_dev_dependencies = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_dev_dependencies =
                line == "[dev-dependencies]" || line.ends_with(".dev-dependencies]");
        }
        if line.contains("p5136-core") {
            assert!(
                in_dev_dependencies,
                "every p5136-core dependency, including aliases and target-specific tables, must be dev-only"
            );
        }
    }
}
