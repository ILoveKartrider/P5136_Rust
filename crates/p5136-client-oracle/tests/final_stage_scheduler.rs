use p5136_client_oracle::final_stage_scheduler::{
    FinalStageMode, IndividualAnimationSignals, IndividualFinalScheduler, IndividualPhase,
    ReadyStage, SchedulerOutcome, TeamFinalScheduler, TeamPhase,
};

const NONE: IndividualAnimationSignals = IndividualAnimationSignals {
    forward_complete: false,
    reverse_complete: false,
};

#[test]
fn ordinary_individual_uses_strict_native_deadlines_then_installs_game_ready() {
    let mut scheduler = IndividualFinalScheduler::new(FinalStageMode::default());
    assert_eq!(scheduler.update(10_000, NONE), SchedulerOutcome::Waiting);
    assert_eq!(scheduler.update(11_000, NONE), SchedulerOutcome::Waiting);
    assert_eq!(scheduler.phase(), IndividualPhase::IntroDelay);

    scheduler.update(11_001, NONE);
    assert_eq!(scheduler.phase(), IndividualPhase::FirstAnimation);
    scheduler.update(
        11_200,
        IndividualAnimationSignals {
            forward_complete: true,
            reverse_complete: false,
        },
    );
    assert_eq!(scheduler.phase(), IndividualPhase::PostFirstAnimation);
    scheduler.update(11_300, NONE);
    assert_eq!(scheduler.phase(), IndividualPhase::PostFirstAnimation);
    scheduler.update(11_301, NONE);
    assert_eq!(scheduler.phase(), IndividualPhase::ReadyDwell);

    assert_eq!(scheduler.update(16_301, NONE), SchedulerOutcome::Waiting);
    assert_eq!(
        scheduler.update(16_302, NONE),
        SchedulerOutcome::InstallReadyStage(ReadyStage::GameReady)
    );
}

#[test]
fn observer_individual_follows_both_animation_holds_and_selects_observer_ready() {
    let mode = FinalStageMode {
        observer: true,
        manual_result_ui: false,
    };
    let mut scheduler = IndividualFinalScheduler::new(mode);
    scheduler.update(20_000, NONE);
    assert_eq!(scheduler.phase(), IndividualPhase::FirstAnimation);
    let both = IndividualAnimationSignals {
        forward_complete: true,
        reverse_complete: true,
    };
    scheduler.update(20_001, both);
    scheduler.update(20_102, both);
    assert_eq!(scheduler.phase(), IndividualPhase::ObserverFirstHold);
    scheduler.update(22_102, both);
    assert_eq!(scheduler.phase(), IndividualPhase::ObserverFirstHold);
    scheduler.update(22_103, both);
    assert_eq!(scheduler.phase(), IndividualPhase::ObserverPanelSetup);
    scheduler.update(22_104, both);
    scheduler.update(22_105, both);
    assert_eq!(scheduler.phase(), IndividualPhase::ObserverSecondHold);
    scheduler.update(24_106, both);
    assert_eq!(scheduler.phase(), IndividualPhase::ReadyDwell);
    assert_eq!(
        scheduler.update(24_107, both),
        SchedulerOutcome::InstallReadyStage(ReadyStage::ObserverReady)
    );
}

#[test]
fn ordinary_team_waits_seven_seconds_after_its_post_animation_phase() {
    let mut scheduler = TeamFinalScheduler::new(FinalStageMode::default());
    scheduler.update(30_000, false);
    scheduler.update(31_001, false);
    assert_eq!(scheduler.phase(), TeamPhase::FirstAnimation);
    scheduler.update(31_002, true);
    assert_eq!(scheduler.phase(), TeamPhase::PostAnimation);
    scheduler.update(31_003, false); // arms the native zero timestamp sentinel
    scheduler.update(31_103, false);
    assert_eq!(scheduler.phase(), TeamPhase::PostAnimation);
    scheduler.update(31_104, false);
    assert_eq!(scheduler.phase(), TeamPhase::ResultDwell);
    scheduler.update(31_105, false); // arms the next phase
    scheduler.update(38_105, false);
    assert_eq!(scheduler.phase(), TeamPhase::ResultDwell);
    scheduler.update(38_106, false);
    assert_eq!(scheduler.phase(), TeamPhase::DispatchReadyStage);
    assert_eq!(
        scheduler.update(38_107, false),
        SchedulerOutcome::InstallReadyStage(ReadyStage::GameReady)
    );
}

#[test]
fn flag_80_team_path_requires_action_13_before_dispatch() {
    let mode = FinalStageMode {
        observer: false,
        manual_result_ui: true,
    };
    let mut scheduler = TeamFinalScheduler::new(mode);
    scheduler.update(40_000, false);
    scheduler.update(41_001, false);
    scheduler.update(41_002, true);
    scheduler.update(41_003, false);
    scheduler.update(41_104, false);
    scheduler.update(41_105, false);
    scheduler.update(48_106, false);
    assert_eq!(scheduler.phase(), TeamPhase::AwaitingManualConfirm);
    assert_eq!(scheduler.update(99_999, false), SchedulerOutcome::Waiting);
    assert!(!scheduler.input(8));
    assert!(scheduler.input(13));
    assert_eq!(
        scheduler.update(100_000, false),
        SchedulerOutcome::InstallReadyStage(ReadyStage::GameReady)
    );
}
