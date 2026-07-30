#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SignalSetValue(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SignalDispositionValue {
    Default,
    Ignore,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignalActionValue {
    pub disposition: SignalDispositionValue,
    pub flags: u32,
    pub mask: SignalSetValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SignalMaskHow {
    Block,
    Unblock,
    Set,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum SignalOperation {
    RegisterThread {
        thread_id: u32,
        inherit_from: u32,
    },
    UnregisterThread {
        thread_id: u32,
    },
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
    UpdateMaskForThread {
        thread_id: u32,
        how: SignalMaskHow,
        set: SignalSetValue,
    },
    Pending,
    BeginDelivery,
    BeginDeliveryForThread {
        thread_id: u32,
    },
    TakePublishedDelivery,
    TakePublishedDeliveryForThread {
        thread_id: u32,
    },
    EndDelivery {
        token: u64,
    },
    EndDeliveryForThread {
        thread_id: u32,
        token: u64,
    },
    BeginTemporaryMask {
        mask: SignalSetValue,
    },
    EndTemporaryMask {
        token: u64,
    },
    BeginTemporaryMaskForThread {
        thread_id: u32,
        mask: SignalSetValue,
    },
    EndTemporaryMaskForThread {
        thread_id: u32,
        token: u64,
    },
}
