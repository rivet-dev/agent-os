#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignalSetValue(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalDispositionValue {
    Default,
    Ignore,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalActionValue {
    pub disposition: SignalDispositionValue,
    pub flags: u32,
    pub mask: SignalSetValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalMaskHow {
    Block,
    Unblock,
    Set,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignalOperation {
    GetAction {
        signal: i32,
    },
    SetAction {
        signal: i32,
        action: SignalActionValue,
    },
    UpdateMask {
        how: SignalMaskHow,
        set: SignalSetValue,
    },
    Pending,
    BeginDelivery,
    TakePublishedDelivery,
    EndDelivery {
        token: u64,
    },
    BeginTemporaryMask {
        mask: SignalSetValue,
    },
    EndTemporaryMask {
        token: u64,
    },
}
