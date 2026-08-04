//! Executable-side podium scheduler reconstructed from the Korean P5136
//! `GameFinalIndiStage` and `GameFinalTeamStage` update functions.
//!
//! This is deliberately separate from packet codecs: after the final-stage
//! packets have installed the podium, the stock client advances these local
//! phases and eventually invokes virtual slot 103 with its saved
//! `GameFinalIndiParam` or `GameFinalParam`. The slot selects a ready-stage
//! class and asks the stage manager to replace the podium.

const INTRO_DELAY_MS: u32 = 1_000;
const POST_ANIMATION_DELAY_MS: u32 = 100;
const INDIVIDUAL_OBSERVER_HOLD_MS: u32 = 2_000;
const INDIVIDUAL_READY_DWELL_MS: u32 = 5_000;
const TEAM_RESULT_DWELL_MS: u32 = 7_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyStage {
    GameReady,
    ObserverReady,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FinalStageMode {
    /// Global game-mode flag `0x40`; the ready-stage selector emits
    /// `ObserverReadyStage` when it is set.
    pub observer: bool,
    /// Global game-mode flag `0x80`; some result presentations wait for local
    /// UI action 13 rather than advancing automatically.
    pub manual_result_ui: bool,
}

impl FinalStageMode {
    const fn ready_stage(self) -> ReadyStage {
        if self.observer {
            ReadyStage::ObserverReady
        } else {
            ReadyStage::GameReady
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndividualAnimationSignals {
    /// Native byte at final-stage offset 2132.
    pub forward_complete: bool,
    /// Native byte at final-stage offset 2133.
    pub reverse_complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerOutcome {
    Waiting,
    InstallReadyStage(ReadyStage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndividualPhase {
    IntroDelay,
    FirstAnimation,
    PostFirstAnimation,
    ObserverFirstHold,
    ObserverPanelSetup,
    ObserverSecondAnimation,
    ObserverSecondHold,
    ReadyDwell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndividualFinalScheduler {
    mode: FinalStageMode,
    phase: IndividualPhase,
    phase_started_at: u32,
    observer_confirm_count: u32,
}

impl IndividualFinalScheduler {
    #[must_use]
    pub const fn new(mode: FinalStageMode) -> Self {
        Self {
            mode,
            phase: IndividualPhase::IntroDelay,
            phase_started_at: 0,
            observer_confirm_count: 0,
        }
    }

    #[must_use]
    pub const fn phase(self) -> IndividualPhase {
        self.phase
    }

    /// Advances one client update tick. Native comparisons are strict `>`,
    /// and tick subtraction is unsigned/wrapping.
    pub fn update(&mut self, now: u32, animation: IndividualAnimationSignals) -> SchedulerOutcome {
        self.arm_zero_timestamp(now);
        match self.phase {
            IndividualPhase::IntroDelay
                if self.mode.observer || self.elapsed(now) > INTRO_DELAY_MS =>
            {
                self.enter_at(IndividualPhase::FirstAnimation, now);
            }
            IndividualPhase::FirstAnimation if animation.forward_complete => {
                self.enter_at(IndividualPhase::PostFirstAnimation, now);
            }
            IndividualPhase::PostFirstAnimation if self.elapsed(now) > POST_ANIMATION_DELAY_MS => {
                let next = if self.mode.observer {
                    IndividualPhase::ObserverFirstHold
                } else {
                    IndividualPhase::ReadyDwell
                };
                self.enter_at(next, now);
            }
            IndividualPhase::ObserverFirstHold
                if animation.reverse_complete
                    && self.elapsed(now) > INDIVIDUAL_OBSERVER_HOLD_MS =>
            {
                self.enter_at(IndividualPhase::ObserverPanelSetup, now);
            }
            IndividualPhase::ObserverPanelSetup => {
                self.observer_confirm_count = 0;
                self.enter_at(IndividualPhase::ObserverSecondAnimation, now);
            }
            IndividualPhase::ObserverSecondAnimation if animation.forward_complete => {
                self.enter_at(IndividualPhase::ObserverSecondHold, now);
            }
            IndividualPhase::ObserverSecondHold
                if animation.reverse_complete
                    && self.elapsed(now) > INDIVIDUAL_OBSERVER_HOLD_MS =>
            {
                self.enter_at(IndividualPhase::ReadyDwell, now);
            }
            IndividualPhase::ReadyDwell
                if self.mode.observer || self.elapsed(now) > INDIVIDUAL_READY_DWELL_MS =>
            {
                return SchedulerOutcome::InstallReadyStage(self.mode.ready_stage());
            }
            _ => {}
        }
        SchedulerOutcome::Waiting
    }

    /// Models only the two local actions that alter the native individual
    /// scheduler while global flag `0x80` is set. Other input delegates to the
    /// common final-stage handler.
    pub fn input(&mut self, action: u32) -> bool {
        if !self.mode.manual_result_ui {
            return false;
        }
        match (self.phase, action) {
            (IndividualPhase::ObserverFirstHold, 13) => {
                self.phase = IndividualPhase::ObserverPanelSetup;
                self.phase_started_at = 0;
                true
            }
            (IndividualPhase::ObserverSecondHold, 8) => {
                self.phase = IndividualPhase::IntroDelay;
                self.phase_started_at = 0;
                true
            }
            (IndividualPhase::ObserverSecondHold, 13) => {
                self.observer_confirm_count = self.observer_confirm_count.saturating_add(1);
                if self.observer_confirm_count > 1 {
                    self.phase = IndividualPhase::ReadyDwell;
                    self.phase_started_at = 0;
                }
                true
            }
            _ => false,
        }
    }

    fn arm_zero_timestamp(&mut self, now: u32) {
        if self.phase_started_at == 0 {
            self.phase_started_at = now;
        }
    }

    fn enter_at(&mut self, phase: IndividualPhase, now: u32) {
        self.phase = phase;
        self.phase_started_at = now;
    }

    const fn elapsed(self, now: u32) -> u32 {
        now.wrapping_sub(self.phase_started_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamPhase {
    IntroDelay,
    FirstAnimation,
    PostAnimation,
    ResultDwell,
    AwaitingManualConfirm,
    DispatchReadyStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamFinalScheduler {
    mode: FinalStageMode,
    phase: TeamPhase,
    phase_started_at: u32,
}

impl TeamFinalScheduler {
    #[must_use]
    pub const fn new(mode: FinalStageMode) -> Self {
        Self {
            mode,
            phase: TeamPhase::IntroDelay,
            phase_started_at: 0,
        }
    }

    #[must_use]
    pub const fn phase(self) -> TeamPhase {
        self.phase
    }

    pub fn update(&mut self, now: u32, animation_complete: bool) -> SchedulerOutcome {
        self.arm_zero_timestamp(now);
        match self.phase {
            TeamPhase::IntroDelay if self.elapsed(now) > INTRO_DELAY_MS => {
                self.enter_unarmed(TeamPhase::FirstAnimation);
            }
            TeamPhase::FirstAnimation if animation_complete => {
                self.enter_unarmed(TeamPhase::PostAnimation);
            }
            TeamPhase::PostAnimation if self.elapsed(now) > POST_ANIMATION_DELAY_MS => {
                self.enter_unarmed(TeamPhase::ResultDwell);
            }
            TeamPhase::ResultDwell if self.elapsed(now) > TEAM_RESULT_DWELL_MS => {
                let next = if self.mode.manual_result_ui {
                    TeamPhase::AwaitingManualConfirm
                } else {
                    TeamPhase::DispatchReadyStage
                };
                self.enter_unarmed(next);
            }
            TeamPhase::DispatchReadyStage => {
                return SchedulerOutcome::InstallReadyStage(self.mode.ready_stage());
            }
            _ => {}
        }
        SchedulerOutcome::Waiting
    }

    /// Native action 13 advances the flag-`0x80` team presentation from phase
    /// 4 to phase 5. No other action changes this scheduler.
    pub fn input(&mut self, action: u32) -> bool {
        if self.mode.manual_result_ui
            && self.phase == TeamPhase::AwaitingManualConfirm
            && action == 13
        {
            self.enter_unarmed(TeamPhase::DispatchReadyStage);
            true
        } else {
            false
        }
    }

    fn arm_zero_timestamp(&mut self, now: u32) {
        if self.phase_started_at == 0 {
            self.phase_started_at = now;
        }
    }

    fn enter_unarmed(&mut self, phase: TeamPhase) {
        self.phase = phase;
        self.phase_started_at = 0;
    }

    const fn elapsed(self, now: u32) -> u32 {
        now.wrapping_sub(self.phase_started_at)
    }
}
