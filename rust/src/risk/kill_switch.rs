/// Emergency kill switch – cancels all open orders and flattens positions.
pub struct KillSwitch {
    active: bool,
}

impl KillSwitch {
    pub fn new() -> Self {
        Self { active: false }
    }

    pub fn activate(&mut self) {
        self.active = true;
        // TODO: cancel all orders, close all positions
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}
