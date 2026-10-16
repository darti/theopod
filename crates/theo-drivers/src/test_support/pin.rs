//! Scripted input pin for button tests.

use std::collections::VecDeque;

use embedded_hal::digital::{ErrorKind, ErrorType, InputPin, PinState};
use embedded_hal_async::digital::Wait;

use super::YieldOnce;

/// Input pin replaying scripted level samples, one per read.
///
/// Level waits are recorded and return at once, as if the level were
/// already reached, while samples remain (one can pend once first, see
/// [`ScriptedPin::pend_wait`]); edge waits are not used by the
/// button.
#[derive(Debug, Default)]
pub struct ScriptedPin {
    /// Levels returned by successive reads; `true` is high.
    pub samples: VecDeque<bool>,
    /// Levels waited for, in order.
    pub waits: Vec<PinState>,
    /// Fail every access once `samples` is exhausted.
    pub fail_when_exhausted: bool,
    /// Leave level waits pending forever once `samples` is exhausted, as a
    /// button that stays where it is.
    pub pend_when_exhausted: bool,
    /// Index of the level wait that returns `Pending` once before it
    /// completes, so a test can move time on while the button waits.
    pub pend_wait: Option<usize>,
}

impl ScriptedPin {
    /// Replays `samples`, `true` meaning high.
    pub fn new(samples: &[bool]) -> Self {
        Self {
            samples: samples.iter().copied().collect(),
            ..Self::default()
        }
    }

    async fn wait_for(&mut self, level: PinState) -> Result<(), ErrorKind> {
        if self.pend_wait == Some(self.waits.len()) {
            self.pend_wait = None;
            YieldOnce(false).await;
        }
        if self.samples.is_empty() && self.fail_when_exhausted {
            return Err(ErrorKind::Other);
        }
        if self.samples.is_empty() && self.pend_when_exhausted {
            self.waits.push(level);
            return core::future::pending().await;
        }
        assert!(
            !self.samples.is_empty(),
            "waiting for a level that never comes"
        );
        self.waits.push(level);
        Ok(())
    }
}

impl ErrorType for ScriptedPin {
    type Error = ErrorKind;
}

impl InputPin for ScriptedPin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        match self.samples.pop_front() {
            Some(level) => Ok(level),
            None if self.fail_when_exhausted => Err(ErrorKind::Other),
            None => panic!("unexpected pin read"),
        }
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        self.is_high().map(|high| !high)
    }
}

impl Wait for ScriptedPin {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        self.wait_for(PinState::High).await
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        self.wait_for(PinState::Low).await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        unimplemented!("the button waits for levels, not edges")
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        unimplemented!("the button waits for levels, not edges")
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        unimplemented!("the button waits for levels, not edges")
    }
}
